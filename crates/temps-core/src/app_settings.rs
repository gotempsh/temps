use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use utoipa::ToSchema;

/// Application settings stored in the database
/// All fields have sensible defaults for easy onboarding
#[derive(Debug, Clone, Serialize, ToSchema, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    // Core settings
    pub external_url: Option<String>,
    /// URL that service containers use to reach the Temps API from *inside*
    /// the Docker network (OTLP metrics ingest, agent callbacks, etc.). On
    /// Docker Desktop this defaults to `http://host.docker.internal:<console_port>`;
    /// on Linux it requires the `host.docker.internal:host-gateway` host
    /// mapping (which Temps adds to provisioned containers). Distinct from
    /// `external_url`, which is the public-facing address.
    pub internal_url: Option<String>,
    pub preview_domain: String,

    // Screenshot settings
    pub screenshots: ScreenshotSettings,

    // TLS/ACME settings
    pub letsencrypt: LetsEncryptSettings,

    // DNS provider settings
    pub dns_provider: DnsProviderSettings,

    // Security settings
    pub security_headers: SecurityHeadersSettings,
    pub rate_limiting: RateLimitSettings,

    // Docker registry settings
    pub docker_registry: DockerRegistrySettings,

    // System monitoring settings
    pub disk_space_alert: DiskSpaceAlertSettings,

    // Docker container log settings
    pub container_logs: ContainerLogSettings,

    // Multi-node settings
    pub multi_node: MultiNodeSettings,

    // Agent sandbox settings (global defaults)
    pub agent_sandbox: AgentSandboxSettings,

    // Workspace preview gateway settings (single shared container per node)
    pub preview_gateway: PreviewGatewaySettings,

    // On-demand (lazy) HTTP-01 TLS issuance settings (ADR-018). Off by default;
    // auto-enabled by `temps setup` for QuickStart (sslip.io) installs.
    pub on_demand_tls: OnDemandTlsSettings,

    // AI configuration settings (global config repo for skills, MCP servers, etc.)
    pub ai_config: AiConfigSettings,

    /// Skip TLS certificate verification on outbound HTTP clients built by the
    /// server (deployer, agent, remote service client). Strictly opt-in for
    /// operators running self-signed control plane / worker certs on a trusted
    /// internal network. Worker→control-plane traffic that traverses the public
    /// internet must keep this `false` — otherwise a MitM steals the join token.
    #[serde(default)]
    pub insecure_tls: bool,

    /// Build-time resource limits applied on the control plane to prevent
    /// `docker build` from saturating host CPU/RAM. Worker nodes are
    /// intentionally NOT subject to these limits (each worker is dedicated
    /// hardware that already has its own per-host headroom).
    pub build_limits: BuildLimitsSettings,

    /// Cluster-DNS resolver settings (ADR-024, experimental beta). Off by
    /// default — see `ClusterDnsSettings` for the incident background and
    /// trade-offs. Must be explicitly enabled by operators who need
    /// `*.temps.local` service-to-service resolution inside containers.
    pub cluster_dns: ClusterDnsSettings,

    /// Metrics observability settings. Controls the MetricsStore backend,
    /// scrape interval, and tiered retention windows.
    pub monitoring: MonitoringSettings,

    /// Set to `true` by `temps setup` (all modes) once initial configuration
    /// has been applied. The web onboarding wizard reads this from the server
    /// and skips itself when true, preventing the "Configure Base Domain" wall
    /// from appearing on installs that were already configured via the CLI.
    #[serde(default)]
    pub setup_complete: bool,

    /// When `true`, any user holding the `Admin` role must have MFA enrolled
    /// (`users.mfa_enabled = true`) to complete a **password** login. Users
    /// without MFA enrolled are rejected with a typed error instructing them
    /// to enroll before retrying. This only gates the password-login path
    /// (`AuthService::login`) -- SSO/OIDC logins are handled by a separate
    /// code path (`OidcService::resolve_user` + `oidc_handler`) and are
    /// intentionally unaffected, since federating identity to a
    /// properly-hardened IdP is itself an acceptable alternative to local
    /// TOTP MFA. Modeled as a settings row (not an env var) per CLAUDE.md so
    /// an operator can flip it at runtime via the Settings API without
    /// restarting the binary.
    #[serde(default)]
    pub require_mfa_for_admins: bool,

    /// Binary version tag (e.g. "v0.1.0") of the *console* process
    /// (`temps serve`, role=all or role=console) that last started. Written
    /// on console startup; read by the standalone `temps proxy` to detect
    /// version skew during a rolling upgrade (ADR-017 Phase 3). `None` on
    /// installs that never ran a console build carrying this field.
    ///
    /// This is informational state written by the binary itself — NOT an
    /// operator-tunable setting. It is intentionally absent from
    /// `AppSettingsResponse` and the PATCH path so an operator cannot
    /// accidentally overwrite the self-recorded value.
    #[serde(default)]
    pub console_version: Option<String>,
}

/// Cluster-DNS resolver settings (ADR-024, experimental beta).
///
/// When `enabled`, the Temps control plane starts a Hickory DNS resolver and
/// injects it as the first nameserver into every deployed container via
/// `HostConfig.Dns` — giving containers the ability to resolve `*.temps.local`
/// FQDNs for service-to-service communication. Worker nodes pick this flag up
/// from the `/api/internal/nodes/{id}/network/peers` wire response and gate
/// their own per-node resolver the same way.
///
/// **Default: `false` (disabled).**
///
/// Why disabled by default: a production incident showed that when the injected
/// Hickory resolver was slow or transiently unresponsive for a non-`*.temps.local`
/// (external) hostname, glibc's resolver cycled through all three nameservers
/// (`172.20.0.1`, `1.1.1.1`, `8.8.8.8`) at ~5 s timeout × 2 attempts each,
/// causing 22–27 s delays for outbound TCP connections. Disabling the injection
/// restores Docker's embedded DNS as the sole resolver, eliminating that failure
/// mode. Operators running single/multi-node installs that depend on
/// `*.temps.local` resolution must explicitly opt in by setting `enabled: true`.
///
/// `bool` defaults to `false` in Rust and JSON (`#[serde(default)]`), so the
/// safe-off behaviour is automatic for new installs and legacy settings rows.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct ClusterDnsSettings {
    /// Master switch. When `false` (default), no custom DNS is injected into
    /// containers — they use Docker's embedded DNS which forwards to the host's
    /// own `resolv.conf`. When `true`, the control-plane Hickory resolver is
    /// started and its bridge IP is injected as the first nameserver so
    /// `*.temps.local` FQDNs resolve inside containers.
    #[schema(example = false)]
    pub enabled: bool,
}

/// Control-plane build resource limits.
///
/// Caps how many builds run concurrently AND how much CPU/memory each build
/// is allowed to consume. A single global semaphore in the deployer crate
/// gates every `DockerRuntime::build_image` call to `max_concurrent`. When
/// the semaphore is full, additional builds queue and wait — they do not
/// fail. Per-build CPU/memory caps are forwarded to Docker via
/// `BuildImageOptions { memory, cpuquota, cpuperiod }`.
///
/// `cpu_limit_cores = 0.0` or `memory_limit_mb = 0` means "no explicit cap"
/// — fall back to the legacy 50%-of-host heuristic for backwards
/// compatibility with operators who never visit the settings page.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct BuildLimitsSettings {
    /// Maximum number of `docker build` operations allowed to run at the
    /// same time on the control plane. Additional builds queue. Min 1.
    #[schema(minimum = 1, example = 2)]
    pub max_concurrent: u32,

    /// CPU cores allowed per build (float, e.g. 2.0 = 2 cores, 0.5 = half
    /// a core). 0 means "use the legacy 50%-of-host default".
    #[schema(minimum = 0.0, example = 2.0)]
    pub cpu_limit_cores: f32,

    /// Memory allowed per build, in megabytes. 0 means "use the legacy
    /// 50%-of-host default". Docker enforces this as a hard cap — builds
    /// that exceed it OOM-kill.
    #[schema(minimum = 0, example = 2048)]
    pub memory_limit_mb: u32,
}

impl Default for BuildLimitsSettings {
    fn default() -> Self {
        Self {
            max_concurrent: 2,
            // 0 = inherit the legacy 50%-of-host heuristic so existing
            // installs see no behaviour change until an operator sets a
            // real value via the settings page.
            cpu_limit_cores: 0.0,
            memory_limit_mb: 0,
        }
    }
}

/// Docker container log rotation settings
/// Controls the `--log-opt max-size` and `--log-opt max-file` for containers
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct ContainerLogSettings {
    /// Maximum size of each log file (e.g., "50m", "100m", "1g")
    /// Docker default is unlimited; we default to "50m" to prevent disk exhaustion
    #[schema(example = "50m")]
    pub max_size: String,
    /// Maximum number of rotated log files to keep (e.g., 3 means up to 3 x max_size total)
    #[schema(example = 3)]
    pub max_file: u32,
    /// Maximum size for external service container logs (postgres, redis, etc.)
    /// Defaults to "20m" since services are typically less verbose than app containers
    #[schema(example = "20m")]
    pub service_max_size: String,
    /// Maximum rotated log files for external service containers
    #[schema(example = 3)]
    pub service_max_file: u32,
}

/// Per-provider credential and configuration entry stored inside
/// `AgentSandboxSettings.providers`. Free-form on purpose: every provider
/// (`claude_cli`, `codex_cli`, `opencode`, future ones) has its own auth
/// model — Claude has subscription-vs-api-key, OpenCode has an arbitrary
/// `auth.json` blob, Codex has a single env var. The Rust-side
/// `ai_cli::catalog` module describes how to interpret each provider's
/// fields, so adding a new provider only requires:
///   1. an entry in the catalog,
///   2. a `seed_provider_credentials` arm in `session_manager`,
///   3. (optionally) UI metadata in the catalog for the settings page.
///
/// No DB migration is ever needed — everything lives inside the existing
/// `settings.data` JSON column.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, Default)]
#[serde(default)]
pub struct ProviderConfig {
    /// Auth flavor for this provider. Valid values depend on the provider:
    ///   - `claude_cli`: "subscription" (OAuth token) | "api_key"
    ///   - `codex_cli`: "api_key"
    ///   - `opencode`:  "config_file"
    pub auth_type: String,
    /// Encrypted credential payload. The decrypted bytes are interpreted
    /// according to the catalog entry's `credential_format`:
    ///   - `ApiKey` / `OauthToken`: plain UTF-8 string (env var value)
    ///   - `ConfigFile`: raw file body written to the catalog's seed path
    pub credentials_encrypted: Option<String>,
    /// Default model id for this provider (e.g. `sonnet` for Claude,
    /// `gpt-5-codex` for Codex). Empty/`None` means "use the CLI's own
    /// default". Each provider uses a disjoint id namespace, so keeping
    /// the default *with* the provider (instead of one global field) means
    /// switching active provider doesn't drop the user into an invalid
    /// model for the new CLI.
    #[serde(default)]
    pub default_model: Option<String>,
    /// Per-provider extras (base URL, custom flags, future per-provider
    /// settings). Intentionally untyped so new providers don't require
    /// schema changes.
    pub extra: serde_json::Value,
}

/// Global agent sandbox settings. Controls whether agent runs are isolated
/// inside Docker containers by default. Individual agents can override this.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct AgentSandboxSettings {
    /// Default AI provider for agents: "claude_cli", "opencode", or "codex_cli".
    /// Workspaces always use this provider — no per-session override.
    #[schema(example = "claude_cli")]
    pub default_provider: String,
    /// Per-provider auth + config. Keyed by provider id (e.g. `claude_cli`,
    /// `codex_cli`, `opencode`). Adding a new provider only requires a new
    /// catalog entry on the Rust side — the JSON column stays migration-free.
    #[serde(default)]
    pub providers: HashMap<String, ProviderConfig>,

    // === Legacy fields (read-only, mirrored into `providers` on load) ===
    // Kept so old settings rows still deserialize. New writes go through
    // `providers`. Removed in a future release once everyone has migrated.
    /// DEPRECATED: use `providers[default_provider].auth_type` instead.
    #[serde(default = "default_auth_type")]
    pub auth_type: String,
    /// DEPRECATED: use `providers[default_provider].credentials_encrypted` instead.
    #[serde(default)]
    pub api_key_encrypted: Option<String>,

    /// Sandbox is always enabled — the executor refuses to run any agent
    /// outside a sandboxed container. Field is retained so existing settings
    /// rows still deserialize, but it is ignored at runtime.
    #[serde(default = "default_sandbox_enabled")]
    pub enabled: bool,
    /// Runtime preset: "node", "bun", "python", "rust", "go", "full", or "custom"
    #[schema(example = "node")]
    pub runtime: String,
    /// Custom Docker image (only used when runtime is "custom").
    /// Must have git and claude CLI installed.
    #[schema(example = "")]
    pub custom_image: String,
    /// CPU limit in cores for sandbox containers
    #[schema(example = 4.0)]
    pub cpu_limit: f64,
    /// Memory limit in MB for sandbox containers
    #[schema(example = 8192)]
    pub memory_limit_mb: u64,
    /// Network access level: "full" (unrestricted), "restricted" (Temps network only), "none" (no network)
    #[schema(example = "full")]
    pub network_mode: String,
}

/// Global AI configuration settings. Controls the default config repo
/// containing `.claude/` directory (skills, MCP servers, plugins) that
/// gets overlaid into every agent sandbox.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct AiConfigSettings {
    /// Global config repo URL in "owner/repo" format (e.g. "myorg/claude-config").
    /// Cloned at agent run time and overlaid into the sandbox's `.claude/` directory.
    #[schema(example = "")]
    pub config_repo: String,
    /// Branch of the config repo to use.
    #[schema(example = "main")]
    pub config_repo_branch: String,
}

impl Default for AiConfigSettings {
    fn default() -> Self {
        Self {
            config_repo: String::new(),
            config_repo_branch: "main".to_string(),
        }
    }
}

fn default_auth_type() -> String {
    "subscription".to_string()
}

fn default_sandbox_enabled() -> bool {
    true
}

impl Default for AgentSandboxSettings {
    fn default() -> Self {
        Self {
            default_provider: "claude_cli".to_string(),
            providers: HashMap::new(),
            auth_type: "subscription".to_string(),
            api_key_encrypted: None,
            enabled: true,
            runtime: "node".to_string(),
            custom_image: String::new(),
            cpu_limit: 4.0,
            memory_limit_mb: 8192,
            network_mode: "full".to_string(),
        }
    }
}

impl AgentSandboxSettings {
    /// Returns the per-provider config, falling back to the deprecated flat
    /// `auth_type` / `api_key_encrypted` fields when the provider entry is
    /// missing. New code reads through this helper so legacy settings rows
    /// keep working without any DB migration.
    pub fn provider_config(&self, provider_id: &str) -> ProviderConfig {
        if let Some(cfg) = self.providers.get(provider_id) {
            return cfg.clone();
        }
        // Legacy fallback. The flat `auth_type` / `api_key_encrypted` fields
        // predate the multi-provider catalog and only ever stored Claude
        // credentials — Codex/OpenCode were added after the `providers` map
        // existed. So we surface the legacy blob under `claude_cli` even
        // when that isn't the currently active provider; otherwise, a user
        // who activates codex loses visibility of their pre-existing Claude
        // credential (and the New-Session picker falsely reports "only one
        // provider configured").
        //
        // We *also* honor it for `default_provider` in case some old install
        // wrote non-Claude credentials into the flat fields via a path we
        // haven't found — cheap insurance, since the only way this differs
        // is if `default_provider != "claude_cli"`, and in that case the
        // flat fields almost certainly hold a Claude credential anyway.
        if provider_id == "claude_cli" || provider_id == self.default_provider {
            return ProviderConfig {
                auth_type: self.auth_type.clone(),
                credentials_encrypted: self.api_key_encrypted.clone(),
                default_model: None,
                extra: serde_json::Value::Null,
            };
        }
        ProviderConfig::default()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct ScreenshotSettings {
    pub enabled: bool,
    pub provider: String,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct LetsEncryptSettings {
    pub email: Option<String>,
    pub environment: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct DnsProviderSettings {
    pub provider: String,
    pub cloudflare_api_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct DockerRegistrySettings {
    pub enabled: bool,
    pub registry_url: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
    pub tls_verify: bool,
    pub ca_certificate: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct SecurityHeadersSettings {
    pub enabled: bool,
    pub preset: String,
    pub content_security_policy: Option<String>,
    pub x_frame_options: String,
    pub x_content_type_options: String,
    pub x_xss_protection: String,
    pub strict_transport_security: String,
    pub referrer_policy: String,
    pub permissions_policy: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct RateLimitSettings {
    pub enabled: bool,
    pub max_requests_per_minute: u32,
    pub max_requests_per_hour: u32,
    pub whitelist_ips: Vec<String>,
    pub blacklist_ips: Vec<String>,
}

/// Disk space alert settings for monitoring disk usage
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct DiskSpaceAlertSettings {
    /// Whether disk space alerts are enabled
    pub enabled: bool,
    /// Threshold percentage (0-100) at which to trigger alerts
    #[schema(minimum = 0, maximum = 100, example = 80)]
    pub threshold_percent: u32,
    /// Interval in seconds between disk space checks
    #[schema(minimum = 60, example = 300)]
    pub check_interval_seconds: u64,
    /// Restrict monitoring to the disk backing this path. When unset (the
    /// default), every mounted writable volume is monitored — including
    /// dedicated volumes such as `/var/lib/docker`.
    pub monitor_path: Option<String>,
}

/// Multi-node cluster settings
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct MultiNodeSettings {
    /// SHA-256 hash of the join token (never store plaintext)
    pub join_token_hash: Option<String>,
    /// Private/WireGuard IP address of the control plane node.
    /// Used by remote worker nodes to reach services (databases, etc.) running on the control plane.
    /// Set via `--private-address` or `TEMPS_PRIVATE_ADDRESS`.
    pub private_address: Option<String>,
    /// Whether the legacy single shared join token is still accepted for node
    /// registration (ADR-020 WS-1.1). Defaults to `true` so existing clusters
    /// keep working on upgrade; fresh installs should set it `false` and rely on
    /// short-lived, single-use enrollment tokens instead.
    #[serde(default = "default_legacy_shared_token_enabled")]
    pub legacy_shared_token_enabled: bool,
    /// Per-cluster CA certificate (PEM) for multi-node mTLS (ADR-020 WS-2.1).
    /// Public — distributed to nodes as the trust root and used by the control
    /// plane as the root for verifying agent server certs. Minted lazily on the
    /// first CSR-bearing registration.
    #[serde(default)]
    pub cluster_ca_cert_pem: Option<String>,
    /// Per-cluster CA private key, AES-256-GCM ciphertext (EncryptionService).
    /// SECRET — never returned over HTTP (elided in the masked response).
    #[serde(default)]
    pub cluster_ca_key_encrypted: Option<String>,
    /// Whether to enforce multi-node mTLS (ADR-020 WS-2.1). When `false`
    /// (default), the control plane ignores join-time CSRs and nodes keep
    /// serving plaintext HTTP — zero behavior change. When `true`, the CP signs
    /// node CSRs, nodes serve mutual TLS, and every CP→agent call uses the
    /// cluster client cert. Observe-then-enforce: flip this on only once all
    /// workers have re-enrolled with certs.
    #[serde(default)]
    pub require_mtls: bool,
    /// CPU-usage percent above which a worker node raises a resource alert
    /// (ADR-020 / monitoring). `None` disables CPU alerting. Default 90.
    #[serde(default = "default_node_cpu_alert_percent")]
    pub node_cpu_alert_percent: Option<f64>,
    /// Memory-usage percent above which a worker node raises a resource alert.
    /// `None` disables memory alerting. Default 90.
    #[serde(default = "default_node_memory_alert_percent")]
    pub node_memory_alert_percent: Option<f64>,
    /// Disk-usage percent above which a worker node raises a resource alert.
    /// `None` disables disk alerting. Default 90.
    #[serde(default = "default_node_disk_alert_percent")]
    pub node_disk_alert_percent: Option<f64>,
}

fn default_node_cpu_alert_percent() -> Option<f64> {
    Some(90.0)
}
fn default_node_memory_alert_percent() -> Option<f64> {
    Some(90.0)
}
fn default_node_disk_alert_percent() -> Option<f64> {
    Some(90.0)
}

fn default_legacy_shared_token_enabled() -> bool {
    true
}

impl Default for MultiNodeSettings {
    fn default() -> Self {
        Self {
            join_token_hash: None,
            private_address: None,
            legacy_shared_token_enabled: true,
            cluster_ca_cert_pem: None,
            cluster_ca_key_encrypted: None,
            require_mtls: false,
            node_cpu_alert_percent: default_node_cpu_alert_percent(),
            node_memory_alert_percent: default_node_memory_alert_percent(),
            node_disk_alert_percent: default_node_disk_alert_percent(),
        }
    }
}

/// Workspace preview gateway settings.
///
/// The preview gateway is a single shared Docker container that lives on the
/// `temps-sandbox-net` network and routes requests to workspace sandbox dev
/// servers based on the `Host` header (`ws-<sid>-<port>.<preview_domain>`).
/// `temps serve` reconciles this container on startup; these settings let an
/// operator override the image, host port, and auto-upgrade behavior.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct PreviewGatewaySettings {
    /// Docker image reference for the gateway. Pinned per Temps release.
    /// Operators can override this to test a custom build.
    #[schema(example = "ghcr.io/gotempsh/temps-preview-gateway:latest")]
    pub image: String,
    /// Host port to publish the gateway on (always bound to 127.0.0.1).
    /// Pingora forwards `ws-*` traffic to this port after authenticating.
    #[schema(example = 8090)]
    pub host_port: u16,
    /// When true (default), the supervisor will pull and apply the image
    /// pinned in the Temps binary on every startup. When false, the
    /// currently-running image is left alone — operators upgrade manually
    /// from the settings UI.
    #[schema(example = true)]
    pub auto_upgrade: bool,
    /// Shared secret the host-side Pingora sends on every forwarded preview
    /// request via `X-Temps-Preview-Token`; the gateway rejects requests
    /// without it. Auto-generated on first boot, persisted in DB so the
    /// secret is stable across `temps serve` restarts regardless of cwd,
    /// `TEMPS_DATA_DIR`, or data-dir changes. MUST be masked (`***`) in any
    /// API response — never expose it over HTTP.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    #[schema(example = "")]
    pub shared_secret: String,
}

impl Default for PreviewGatewaySettings {
    fn default() -> Self {
        Self {
            image: "ghcr.io/gotempsh/temps-preview-gateway:latest".to_string(),
            host_port: 8090,
            auto_upgrade: true,
            shared_secret: String::new(),
        }
    }
}

/// On-demand (lazy) HTTP-01 TLS issuance settings (ADR-018).
///
/// When `enabled`, the proxy's `certificate_callback` triggers ACME HTTP-01
/// issuance for allowlisted, STABLE hostnames (per-environment aliases and the
/// console host) that have no active cert, rather than silently failing the
/// handshake. Ephemeral per-deployment hostnames are NEVER certed (ADR §2).
///
/// Off by default — operators opt in explicitly, except QuickStart (`sslip.io`)
/// installs where `temps setup` auto-enables it and derives `zone`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct OnDemandTlsSettings {
    /// Master switch. When `false` (default) the proxy's on-demand cert gate
    /// rejects every SNI and no issuance is ever triggered.
    #[schema(example = false)]
    pub enabled: bool,

    /// Zone suffix for the allowlist gate. A hostname passes the gate only if
    /// it is a direct subdomain of this zone (e.g. zone `1.2.3.4.sslip.io`
    /// admits `myapp.1.2.3.4.sslip.io` but not `deep.sub.1.2.3.4.sslip.io`).
    /// `None` (default) means "auto-derive from `external_url`"; if no zone can
    /// be derived the gate rejects all SNI, disabling the feature.
    #[schema(example = "1.2.3.4.sslip.io")]
    pub zone: Option<String>,

    /// Maximum number of ACME issuance flows allowed to run simultaneously
    /// (the concurrent-issuance semaphore, ADR §4 Layer 1). Min 1.
    #[schema(minimum = 1, example = 3)]
    pub max_concurrent: u32,

    /// Global cap on total on-demand issuances per hour across all hostnames
    /// (ADR §4 Layer 3). The operator's self-imposed safety net, separate from
    /// the Let's Encrypt rate limit.
    #[schema(minimum = 1, example = 10)]
    pub hourly_cap: u32,

    /// How ephemeral per-deployment hostnames behave when they have no cert
    /// (they are NEVER certed — see ADR §2). One of:
    ///   - `"http"` (default): serve plain HTTP on :80.
    ///   - `"redirect_to_env"`: 308-redirect to the stable per-environment URL,
    ///     which IS certed.
    #[schema(example = "http")]
    pub deployment_url_mode: String,
}

impl Default for OnDemandTlsSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            zone: None,
            max_concurrent: 3,
            hourly_cap: 10,
            deployment_url_mode: "http".to_string(),
        }
    }
}

// ============================================================
// Monitoring / metrics settings
// ============================================================

/// Which storage backend to use for the MetricsStore.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MetricsStoreKind {
    /// Default: TimescaleDB (same PostgreSQL instance used by the control plane).
    TimescaleDb,
    /// Optional: ClickHouse cluster — requires `clickhouse_url` to be set.
    ClickHouse,
}

/// Global metrics observability configuration.
///
/// Controls whether the MetricsScraper and AlertEvaluator background tasks
/// are active, which storage backend they write to, and how long data is kept
/// at each retention tier.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct MonitoringSettings {
    /// Enable or disable all metrics collection (scraping + alerting).
    /// Defaults to `false` so new installs don't write to TimescaleDB until
    /// an operator explicitly enables the feature.
    pub enabled: bool,

    /// Storage backend for metric data.
    pub store: MetricsStoreKind,

    /// How often the MetricsScraper collects data from all sources, in seconds.
    /// Minimum effective value is 10 s; values below that are clamped at runtime.
    #[schema(minimum = 10, example = 30)]
    pub scrape_interval_secs: u64,

    /// How many days of raw (30 s resolution) metric data to keep.
    #[schema(minimum = 1, example = 7)]
    pub retention_raw_days: u32,

    /// How many days of hourly-aggregate data to keep.
    #[schema(minimum = 1, example = 90)]
    pub retention_hourly_days: u32,

    /// How many years of daily-aggregate data to keep (converted to days internally).
    #[schema(minimum = 1, example = 2)]
    pub retention_daily_years: u32,

    /// ClickHouse DSN, required only when `store = "click_house"`.
    /// Example: `"http://localhost:8123"`.
    pub clickhouse_url: Option<String>,
}

impl Default for MonitoringSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            store: MetricsStoreKind::TimescaleDb,
            scrape_interval_secs: 30,
            retention_raw_days: 7,
            retention_hourly_days: 90,
            retention_daily_years: 2,
            clickhouse_url: None,
        }
    }
}

const DEFAULT_LOCAL_DOMAIN: &str = "localho.st";
impl Default for AppSettings {
    fn default() -> Self {
        Self {
            external_url: None,
            internal_url: None,
            preview_domain: DEFAULT_LOCAL_DOMAIN.to_string(),
            screenshots: ScreenshotSettings::default(),
            letsencrypt: LetsEncryptSettings::default(),
            dns_provider: DnsProviderSettings::default(),
            security_headers: SecurityHeadersSettings::default(),
            rate_limiting: RateLimitSettings::default(),
            docker_registry: DockerRegistrySettings::default(),
            disk_space_alert: DiskSpaceAlertSettings::default(),
            container_logs: ContainerLogSettings::default(),
            multi_node: MultiNodeSettings::default(),
            agent_sandbox: AgentSandboxSettings::default(),
            preview_gateway: PreviewGatewaySettings::default(),
            on_demand_tls: OnDemandTlsSettings::default(),
            ai_config: AiConfigSettings::default(),
            insecure_tls: false,
            build_limits: BuildLimitsSettings::default(),
            cluster_dns: ClusterDnsSettings::default(),
            monitoring: MonitoringSettings::default(),
            setup_complete: false,
            require_mfa_for_admins: false,
            console_version: None,
        }
    }
}

impl Default for ContainerLogSettings {
    fn default() -> Self {
        Self {
            max_size: "50m".to_string(),
            max_file: 3,
            service_max_size: "20m".to_string(),
            service_max_file: 3,
        }
    }
}

impl Default for ScreenshotSettings {
    fn default() -> Self {
        Self {
            enabled: false, // Disabled by default as requested
            provider: "local".to_string(),
            url: "".to_string(),
        }
    }
}

impl Default for LetsEncryptSettings {
    fn default() -> Self {
        Self {
            email: None,
            environment: "production".to_string(),
        }
    }
}

impl Default for DnsProviderSettings {
    fn default() -> Self {
        Self {
            provider: "manual".to_string(),
            cloudflare_api_key: None,
        }
    }
}

impl Default for DockerRegistrySettings {
    fn default() -> Self {
        Self {
            enabled: false,
            registry_url: None,
            username: None,
            password: None,
            tls_verify: true,
            ca_certificate: None,
        }
    }
}

impl Default for SecurityHeadersSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            preset: "moderate".to_string(),
            content_security_policy: Some(
                "default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' data: https:; font-src 'self' data:; connect-src 'self'; frame-ancestors 'self'".to_string()
            ),
            x_frame_options: "SAMEORIGIN".to_string(),
            x_content_type_options: "nosniff".to_string(),
            x_xss_protection: "1; mode=block".to_string(),
            strict_transport_security: "max-age=31536000; includeSubDomains".to_string(),
            referrer_policy: "strict-origin-when-cross-origin".to_string(),
            permissions_policy: Some("geolocation=(), microphone=(), camera=()".to_string()),
        }
    }
}

impl Default for RateLimitSettings {
    fn default() -> Self {
        Self {
            enabled: false, // Disabled by default for initial setup
            max_requests_per_minute: 60,
            max_requests_per_hour: 1000,
            whitelist_ips: vec![],
            blacklist_ips: vec![],
        }
    }
}

impl Default for DiskSpaceAlertSettings {
    fn default() -> Self {
        Self {
            enabled: true,               // Enabled by default
            threshold_percent: 80,       // Alert at 80% usage
            check_interval_seconds: 300, // Check every 5 minutes
            monitor_path: None,          // Monitor all mounted disks by default
        }
    }
}

impl SecurityHeadersSettings {
    /// Strict preset for maximum security
    pub fn strict() -> Self {
        Self {
            enabled: true,
            preset: "strict".to_string(),
            content_security_policy: Some(
                "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; font-src 'self'; connect-src 'self'; frame-ancestors 'none'".to_string()
            ),
            x_frame_options: "DENY".to_string(),
            x_content_type_options: "nosniff".to_string(),
            x_xss_protection: "1; mode=block".to_string(),
            strict_transport_security: "max-age=63072000; includeSubDomains; preload".to_string(),
            referrer_policy: "no-referrer".to_string(),
            permissions_policy: Some("geolocation=(), microphone=(), camera=(), payment=(), usb=()".to_string()),
        }
    }

    /// Permissive preset for development/compatibility
    pub fn permissive() -> Self {
        Self {
            enabled: true,
            preset: "permissive".to_string(),
            content_security_policy: Some(
                "default-src *; script-src * 'unsafe-inline' 'unsafe-eval'; style-src * 'unsafe-inline'; img-src * data:; font-src * data:".to_string()
            ),
            x_frame_options: "SAMEORIGIN".to_string(),
            x_content_type_options: "nosniff".to_string(),
            x_xss_protection: "1; mode=block".to_string(),
            strict_transport_security: "max-age=31536000".to_string(),
            referrer_policy: "no-referrer-when-downgrade".to_string(),
            permissions_policy: None,
        }
    }

    /// Disabled preset (no security headers)
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            preset: "disabled".to_string(),
            content_security_policy: None,
            x_frame_options: String::new(),
            x_content_type_options: String::new(),
            x_xss_protection: String::new(),
            strict_transport_security: String::new(),
            referrer_policy: String::new(),
            permissions_policy: None,
        }
    }
}

impl AppSettings {
    /// Create settings from JSON value, using defaults for missing fields
    pub fn from_json(value: serde_json::Value) -> Self {
        serde_json::from_value(value).unwrap_or_default()
    }

    /// Convert settings to JSON value
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_else(|_| serde_json::json!({}))
    }

    /// Resolve the URL that service containers use to reach the Temps API from
    /// inside the Docker network. Resolution order:
    ///   1. `internal_url` settings field (admin-editable, runtime)
    ///   2. `TEMPS_INTERNAL_API_URL` env var (operator override at startup)
    ///   3. `http://host.docker.internal:{console_port}` default
    ///
    /// The returned value has no trailing slash. `console_port` is the port the
    /// API/console listener binds to (callers pass it from `ServerConfig`).
    pub fn resolve_internal_url(&self, console_port: u16) -> String {
        let raw = self
            .internal_url
            .clone()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| {
                std::env::var("TEMPS_INTERNAL_API_URL")
                    .ok()
                    .filter(|s| !s.is_empty())
            })
            .unwrap_or_else(|| format!("http://host.docker.internal:{console_port}"));
        raw.trim_end_matches('/').to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ADR-024: cluster-DNS injection is experimental/beta and defaults OFF
    // to avoid the DNS-timeout-cascade failure mode (22-27 s TCP delays when
    // the injected resolver is transiently slow for external hostnames).
    #[test]
    fn cluster_dns_defaults_disabled() {
        let s = ClusterDnsSettings::default();
        assert!(
            !s.enabled,
            "cluster DNS must be opt-in (off by default) to avoid DNS cascade delays"
        );
    }

    #[test]
    fn app_settings_default_has_cluster_dns_disabled() {
        let s = AppSettings::default();
        assert!(
            !s.cluster_dns.enabled,
            "AppSettings::default() must have cluster_dns.enabled = false"
        );
    }

    #[test]
    fn cluster_dns_round_trips_through_json() {
        let mut s = AppSettings::default();
        s.cluster_dns.enabled = true;

        let json = s.to_json();
        let back = AppSettings::from_json(json);
        assert!(
            back.cluster_dns.enabled,
            "cluster_dns.enabled must survive JSON round-trip"
        );
    }

    #[test]
    fn legacy_settings_json_without_cluster_dns_deserializes_as_disabled() {
        // Old `settings.data` rows have no `cluster_dns` key. `#[serde(default)]`
        // must fill it in with the disabled default so pre-ADR-024 rows keep
        // loading and the feature stays off.
        let legacy = serde_json::json!({
            "external_url": "https://paas.example.com",
            "preview_domain": "localho.st"
        });
        let parsed = AppSettings::from_json(legacy);
        assert!(
            !parsed.cluster_dns.enabled,
            "cluster_dns must default to disabled when deserializing a legacy settings row"
        );
    }

    #[test]
    fn on_demand_tls_defaults_are_off_and_sensible() {
        let s = OnDemandTlsSettings::default();
        assert!(!s.enabled, "on-demand TLS must be opt-in (off by default)");
        assert_eq!(s.zone, None);
        assert_eq!(s.max_concurrent, 3);
        assert_eq!(s.hourly_cap, 10);
        assert_eq!(s.deployment_url_mode, "http");
    }

    #[test]
    fn app_settings_default_includes_on_demand_tls_disabled() {
        let s = AppSettings::default();
        assert!(!s.on_demand_tls.enabled);
        assert_eq!(s.on_demand_tls.deployment_url_mode, "http");
    }

    #[test]
    fn legacy_settings_json_without_on_demand_tls_deserializes() {
        // An old `settings.data` row written before ADR-018 has no
        // `on_demand_tls` key. `#[serde(default)]` must fill it in with the
        // disabled default so pre-migration rows keep loading.
        let legacy = serde_json::json!({
            "external_url": "https://paas.example.com",
            "preview_domain": "localho.st"
        });
        let parsed = AppSettings::from_json(legacy);
        assert_eq!(
            parsed.external_url.as_deref(),
            Some("https://paas.example.com")
        );
        assert!(!parsed.on_demand_tls.enabled);
        assert_eq!(parsed.on_demand_tls.max_concurrent, 3);
        assert_eq!(parsed.on_demand_tls.hourly_cap, 10);
    }

    #[test]
    fn on_demand_tls_round_trips_through_json() {
        let mut s = AppSettings::default();
        s.on_demand_tls.enabled = true;
        s.on_demand_tls.zone = Some("1.2.3.4.sslip.io".to_string());
        s.on_demand_tls.max_concurrent = 5;
        s.on_demand_tls.hourly_cap = 25;
        s.on_demand_tls.deployment_url_mode = "redirect_to_env".to_string();

        let json = s.to_json();
        let back = AppSettings::from_json(json);
        assert!(back.on_demand_tls.enabled);
        assert_eq!(back.on_demand_tls.zone.as_deref(), Some("1.2.3.4.sslip.io"));
        assert_eq!(back.on_demand_tls.max_concurrent, 5);
        assert_eq!(back.on_demand_tls.hourly_cap, 25);
        assert_eq!(back.on_demand_tls.deployment_url_mode, "redirect_to_env");
    }

    #[test]
    fn require_mfa_for_admins_defaults_to_false() {
        // MFA enforcement must be opt-in: an operator upgrading Temps should
        // never suddenly get locked out of their own Admin account because a
        // new default flipped a login-blocking setting on.
        let s = AppSettings::default();
        assert!(!s.require_mfa_for_admins);
    }

    #[test]
    fn legacy_settings_json_without_require_mfa_for_admins_deserializes() {
        // A `settings.data` row written before this feature shipped has no
        // `require_mfa_for_admins` key. `#[serde(default)]` must fill it in
        // with `false` so pre-migration rows keep loading and don't
        // retroactively lock out admins who never enrolled MFA.
        let legacy = serde_json::json!({
            "external_url": "https://paas.example.com",
            "preview_domain": "localho.st"
        });
        let parsed = AppSettings::from_json(legacy);
        assert!(!parsed.require_mfa_for_admins);
    }

    #[test]
    fn require_mfa_for_admins_round_trips_through_json() {
        let s = AppSettings {
            require_mfa_for_admins: true,
            ..AppSettings::default()
        };

        let json = s.to_json();
        let back = AppSettings::from_json(json);
        assert!(back.require_mfa_for_admins);
    }
}
