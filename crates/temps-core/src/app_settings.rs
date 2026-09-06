// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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
    /// Public edge target that generated DNS records point at when a managed
    /// domain opts into automatic record sync. An IPv4/IPv6 address produces an
    /// `A`/`AAAA` record; anything else is treated as a `CNAME` target. `None`
    /// disables DNS record sync regardless of per-domain opt-in.
    pub edge_target: Option<String>,

    /// Whether plain-HTTP requests to the console host (`external_url`) are
    /// redirected to HTTPS. Same tri-state contract as an environment's
    /// `force_https`:
    ///
    /// - `None` (default) — inherit the per-host heuristic: redirect only once
    ///   the console hostname has actually completed TLS provisioning. An
    ///   HTTP-only install, and an install whose TLS is terminated upstream,
    ///   are both left alone.
    /// - `Some(true)` — always redirect. For operators who terminate TLS at
    ///   Temps but have not provisioned the cert through Temps.
    /// - `Some(false)` — never redirect, even once a certificate exists.
    ///
    /// Deliberately operator-set rather than inferred. Temps cannot tell
    /// "HTTPS terminated by an upstream CDN" apart from "plain HTTP" — both
    /// arrive as a plaintext connection, and `X-Forwarded-Proto` is not
    /// trustworthy from an arbitrary peer — so inferring `true` from an
    /// `https://` `external_url` would 301 a CDN-fronted console into an
    /// infinite redirect loop with no way out but the global kill switch.
    #[serde(default)]
    pub console_force_https: Option<bool>,

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

    /// Prefix applied to Docker Hub base images generated for a build (e.g.
    /// autopack's `FROM node:22-slim`), turning them into
    /// `{prefix}/node:22-slim`. Unlike `docker_registry` above — which
    /// authenticates pulls to one *named* private registry a user's own image
    /// reference already points at — this rewrites Temps' own generated,
    /// otherwise-anonymous `docker.io` references, for operators whose
    /// internal registry is a path-prefixing reverse proxy rather than a
    /// `registry-mirrors`-compatible pull-through cache (which needs no
    /// rewriting at all — see docs/howto/configure-a-docker-registry-mirror).
    /// `None`/empty (the default) leaves every reference untouched.
    #[serde(default)]
    pub registry_mirror_prefix: Option<String>,

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

    /// Limits on a single AI chat turn. Operator-tunable because the right
    /// value depends on the model: a turn against a slow self-hosted model can
    /// legitimately take ten minutes, while a hosted one finishes in seconds
    /// and a shorter ceiling keeps costs predictable.
    #[serde(default)]
    pub ai_chat_limits: AiChatLimitsSettings,

    /// Upstream request/connection timeouts applied by the proxy to customer
    /// app traffic. Provides a global hard ceiling plus global defaults for
    /// regular HTTP, SSE, and WebSocket traffic; projects and environments
    /// may set a shorter value but never exceed the ceiling here.
    #[serde(default)]
    pub request_timeouts: RequestTimeoutSettings,

    /// Per-upstream concurrent-connection cap applied by the proxy to
    /// customer app traffic. `0` (the default) is unlimited. See issue #646.
    #[serde(default)]
    pub connection_limits: ConnectionLimitSettings,

    /// Ceilings the operator places on what a *tenant* may configure for
    /// their own project/environment. Entirely unenforced by default, so an
    /// upgrade never changes what an existing config means.
    #[serde(default)]
    pub tenant_resource_ceilings: TenantResourceCeilings,

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

    /// Retention policy for locally-built deployment images. Modeled as a
    /// settings row (not an env var) per CLAUDE.md so an operator can change
    /// the system-wide default at runtime without restarting the binary.
    /// Individual projects override it via `projects.image_retention_hours`.
    pub image_retention: ImageRetentionSettings,

    /// Cluster-DNS resolver settings (ADR-024, experimental beta). Off by
    /// default — see `ClusterDnsSettings` for the incident background and
    /// trade-offs. Must be explicitly enabled by operators who need
    /// `*.temps.local` service-to-service resolution inside containers.
    pub cluster_dns: ClusterDnsSettings,

    /// Metrics observability settings. Controls the MetricsStore backend,
    /// scrape interval, and tiered retention windows.
    pub monitoring: MonitoringSettings,

    /// TimescaleDB compression delays for immutable observability data.
    /// Changes are applied at runtime by the Settings API.
    pub observability_compression: ObservabilityCompressionSettings,

    /// Retention windows for raw proxy and OpenTelemetry telemetry.
    /// TimescaleDB policies are updated at runtime by the Settings API.
    pub observability_retention: ObservabilityRetentionSettings,

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

    /// One-click "Update now" from the console. Enabled by default; an admin
    /// can turn it off here to keep upgrades on the CLI/config-management path.
    ///
    /// This is the *soft* switch — it is stored in the database, so whoever can
    /// write settings can also turn it back on. Operators who need an upgrade
    /// path that no console session can re-open should start the server with
    /// `--disable-self-update`, which wins over this field unconditionally.
    /// `None` means the client did not express an opinion, NOT "reset to
    /// default". Every other field on this struct is safe to re-default on a
    /// partial write, but this one gates whether the server may replace its own
    /// binary — silently flipping it back on because an older client PUT a
    /// settings document without it would undo a deliberate security decision.
    /// The update handler preserves the stored value when this is absent; read
    /// it through `self_update()`.
    #[serde(default)]
    pub self_update: Option<SelfUpdateSettings>,

    /// MCP server settings (ADR-039). Off by default — enable via the Settings
    /// UI to expose the MCP endpoint to the Temps CLI wizard.
    #[serde(default)]
    pub mcp_server: McpServerSettings,

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

/// MCP server settings (ADR-039).
///
/// The MCP endpoint lets AI tools (e.g. the Temps CLI wizard) interact with
/// this Temps instance through the Model Context Protocol.  Disabled by
/// default so new installs do not expose the endpoint until the operator
/// explicitly opts in.
///
/// `bool` defaults to `false` in Rust and JSON (`#[serde(default)]`), so the
/// safe-off behaviour is automatic for new installs and legacy settings rows.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct McpServerSettings {
    /// Master switch. When `false` (default), `GET /mcp/tools` returns `404`
    /// and all other MCP endpoints return `404` too.  Set to `true` via the
    /// Settings UI to activate the MCP server.
    #[schema(example = false)]
    pub enabled: bool,
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

/// Bounds on one AI chat turn.
///
/// A turn is bounded by TIME rather than by a number of steps. A step count
/// says nothing about cost or about how long someone has been watching a
/// spinner, and it cuts short exactly the long, productive turns the chat
/// exists for. The user can already see each tool call and press Stop; the
/// deadline is what guarantees an *unattended* turn still ends.
///
/// The right value is a property of the model, which is why it is configurable
/// rather than compiled in: a full alert-suggestion turn takes ~10 minutes
/// against a slow local model and seconds against a hosted one.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct AiChatLimitsSettings {
    /// How long one turn may run before it is stopped and the partial answer
    /// returned, in seconds. The user is told the turn was cut short.
    ///
    /// Checked between steps, not mid-call: a model round already in flight
    /// finishes, so a turn can overrun by up to one round. Against a slow
    /// self-hosted model that is a minute or two. Aborting mid-stream would cut
    /// the answer off in the middle of a sentence and throw away work already
    /// paid for, which is worse than a late stop.
    #[schema(minimum = 30, maximum = 3600, example = 900)]
    pub turn_timeout_secs: u32,
}

impl Default for AiChatLimitsSettings {
    fn default() -> Self {
        Self {
            // Generous against a full alert-suggestion turn on a slow local
            // model (~10 min) while capping what a single message can cost.
            turn_timeout_secs: 15 * 60,
        }
    }
}

impl AiChatLimitsSettings {
    /// Lower bound: below this a turn cannot complete even simple tool work,
    /// so accepting it would just look like the chat is broken.
    pub const MIN_TURN_TIMEOUT_SECS: u32 = 30;
    /// Upper bound: an hour of provider calls from one message is already far
    /// past anything useful, and the value is a cost ceiling.
    pub const MAX_TURN_TIMEOUT_SECS: u32 = 3600;

    /// The configured timeout, clamped to the supported range.
    ///
    /// Clamped rather than trusted: the settings row is JSON that predates this
    /// field and can be written by any admin, and a zero would otherwise mean
    /// "every turn times out instantly".
    pub fn turn_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(
            self.turn_timeout_secs
                .clamp(Self::MIN_TURN_TIMEOUT_SECS, Self::MAX_TURN_TIMEOUT_SECS) as u64,
        )
    }
}

/// Upstream request/connection timeouts for customer app traffic.
///
/// By default, no timeout is applied to customer app traffic at all — an
/// existing app that happens to have a slow endpoint, a long-polling
/// request, or an unusually long response must keep working exactly as it
/// did before this setting existed. Timeouts here are opt-in: an operator
/// can set a global default, and/or a project/environment can set its own
/// override (`DeploymentConfig::request_timeout_seconds` /
/// `sse_idle_timeout_seconds` / `websocket_idle_timeout_seconds`), but until
/// one of those is explicitly configured, the proxy holds the connection
/// open indefinitely (bounded only by TCP/OS-level limits).
///
/// `default_*_timeout_seconds` of `0` means "no timeout" — this is the
/// out-of-the-box value for all three. `max_request_timeout_seconds` is a
/// hard ceiling that only comes into play once a timeout is actually
/// configured (globally or per project/environment): whatever value is
/// resolved is always clamped to it, so lowering the ceiling here takes
/// effect immediately without needing every environment row re-saved. It
/// never *creates* a timeout for traffic that has none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct RequestTimeoutSettings {
    /// Hard ceiling, in seconds, applied once a timeout is configured (via a
    /// global default above or a project/environment override). Has no
    /// effect on traffic with no timeout configured at all.
    #[schema(minimum = 5, maximum = 86400, example = 600)]
    pub max_request_timeout_seconds: u32,

    /// Default timeout for regular (non-streaming) HTTP requests, in
    /// seconds. Used when a project/environment hasn't set
    /// `request_timeout_seconds`. `0` (the default) means no timeout.
    #[schema(minimum = 0, example = 0)]
    pub default_http_timeout_seconds: u32,

    /// Default idle timeout for Server-Sent Events streams, in seconds. Used
    /// when a project/environment hasn't set `sse_idle_timeout_seconds`. `0`
    /// (the default) means no timeout.
    #[schema(minimum = 0, example = 0)]
    pub default_sse_idle_timeout_seconds: u32,

    /// Default idle timeout for WebSocket connections, in seconds. Used when
    /// a project/environment hasn't set `websocket_idle_timeout_seconds`.
    /// `0` (the default) means no timeout.
    #[schema(minimum = 0, example = 0)]
    pub default_websocket_idle_timeout_seconds: u32,
}

impl RequestTimeoutSettings {
    /// Lower bound for `max_request_timeout_seconds`: below this, ordinary
    /// requests to a slow-starting app would routinely fail.
    pub const MIN_CEILING_SECS: u32 = 5;
    /// Upper bound for `max_request_timeout_seconds`: a day-long single
    /// upstream connection is already far past anything a proxy should hold
    /// open.
    pub const MAX_CEILING_SECS: u32 = 86400;

    /// The configured ceiling, clamped to the supported range. Clamped
    /// rather than trusted for the same reason as `AiChatLimitsSettings`:
    /// the settings row is JSON any admin can write, and an unclamped 0
    /// would mean "every request times out instantly" for any traffic that
    /// does have a timeout configured.
    pub fn ceiling(&self) -> u32 {
        self.max_request_timeout_seconds
            .clamp(Self::MIN_CEILING_SECS, Self::MAX_CEILING_SECS)
    }

    /// Clamp a resolved, *already-nonzero* per-request timeout (merged from
    /// project/environment overrides or one of the defaults above) down to
    /// the hard ceiling. The ceiling always wins. Callers must treat `0`
    /// (no timeout) as a distinct case and never pass it here — clamping
    /// would turn "no timeout" into "the ceiling," which is exactly the
    /// unwanted default-on behavior this type exists to avoid.
    pub fn clamp_to_ceiling(&self, seconds: u32) -> u32 {
        seconds.min(self.ceiling())
    }
}

impl Default for RequestTimeoutSettings {
    fn default() -> Self {
        Self {
            max_request_timeout_seconds: 600,
            default_http_timeout_seconds: 0,
            default_sse_idle_timeout_seconds: 0,
            default_websocket_idle_timeout_seconds: 0,
        }
    }
}

/// Per-upstream concurrent-connection limiting. Protects the proxy's own
/// connection/file-descriptor budget from a single slow or malicious
/// customer upstream — independent of the request/idle timeouts in
/// `RequestTimeoutSettings`, which bound how long a connection may stay
/// open, not how many may exist at once. See issue #646.
#[derive(Debug, Clone, PartialEq, Default, Serialize, ToSchema, Deserialize)]
#[serde(default)]
pub struct ConnectionLimitSettings {
    /// Default max concurrent in-flight requests to a single
    /// project/environment's upstream, used when the project/environment
    /// hasn't set its own `max_concurrent_connections` override. `0` (the
    /// default) means unlimited — matches the "opt-in, never breaks an
    /// existing app on upgrade" philosophy already established for
    /// `RequestTimeoutSettings`.
    #[schema(minimum = 0, example = 200)]
    pub default_max_concurrent_connections: u32,
}

/// Ceilings on the resource overrides a *tenant* may set for their own
/// project or environment.
///
/// The knobs these bound (`memory_limit`, `max_concurrent_connections`, the
/// request/idle timeouts) are deliberately uncapped-by-sentinel: `0` means
/// "unlimited". That is the right default for a single-team self-hosted
/// install, where the person editing a project *is* the operator. It is the
/// wrong default on a shared host, where it lets one project opt out of the
/// operator's protection and take the node — or the shared proxy's connection
/// budget — down with it.
///
/// Every ceiling here is therefore **off by default**, and turning one on is
/// what makes the corresponding tenant override enforceable. A caller holding
/// `Permission::SettingsWrite` (operators: `Admin`/`PlatformAdmin`, never
/// `Role::User`) may still exceed them — the ceiling constrains tenants, not
/// the operator who set it.
///
/// Violations are **rejected, not clamped**: silently rewriting a value the
/// user asked for leaves them debugging a limit they believe they removed,
/// and self-hosted operators have no support channel to ask.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct TenantResourceCeilings {
    /// Largest `memory_limit` (MB) a project/environment may set for its
    /// containers. `0` (the default) leaves it unenforced.
    ///
    /// A project value of `0` means "no cgroup limit at all", so it is
    /// refused whenever this ceiling is set — that is the case that OOMs the
    /// host, not merely a large number.
    #[schema(minimum = 0, example = 4096)]
    pub max_memory_limit_mb: u32,

    /// Largest `max_concurrent_connections` a project/environment may set.
    /// `0` (the default) leaves it unenforced.
    ///
    /// As with memory, a project value of `0` means unlimited and is refused
    /// whenever this ceiling is set.
    #[schema(minimum = 0, example = 200)]
    pub max_concurrent_connections: u32,

    /// Whether a project/environment may set a request, SSE or WebSocket
    /// timeout of `0` ("no timeout"). `true` (the default) preserves current
    /// behaviour.
    ///
    /// Nonzero tenant timeouts need no ceiling here: they are already clamped
    /// to [`RequestTimeoutSettings::ceiling`] at resolution time. `0` escapes
    /// that clamp by construction — it means "no timeout is configured", so
    /// there is nothing to clamp — which is precisely the hole this closes.
    pub allow_unlimited_request_timeouts: bool,
}

/// Hand-written rather than derived: `#[derive(Default)]` would make
/// `allow_unlimited_request_timeouts` **false**, which is the opposite of the
/// "an upgrade changes nothing" contract — it would start rejecting the `0`
/// timeouts that are currently the documented default for every traffic class.
impl Default for TenantResourceCeilings {
    fn default() -> Self {
        Self {
            max_memory_limit_mb: 0,
            max_concurrent_connections: 0,
            allow_unlimited_request_timeouts: true,
        }
    }
}

impl TenantResourceCeilings {
    /// True when no ceiling is configured, i.e. tenants are unconstrained and
    /// validation can be skipped entirely.
    pub fn is_unenforced(&self) -> bool {
        self.max_memory_limit_mb == 0
            && self.max_concurrent_connections == 0
            && self.allow_unlimited_request_timeouts
    }
}

/// Whether a deployment-config write should be checked against
/// [`TenantResourceCeilings`].
///
/// Handlers compute this from the caller's `SettingsWrite` permission —
/// whoever can raise the ceilings is by definition allowed to exceed them,
/// so the check would be theatre for them — and pass it down to the service
/// layer, which has no access to `auth` itself. A bare `bool` parameter
/// here previously left call sites (and test fixtures) needing a comment to
/// say what `true`/`false` meant; the variant names make that self-evident.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CeilingEnforcement {
    /// Check the write against `AppSettings.tenant_resource_ceilings`.
    Enforce,
    /// Skip the check — the caller holds `Permission::SettingsWrite`.
    Bypass,
}

impl CeilingEnforcement {
    /// The permission that grants the bypass, wrapped as a variant.
    pub fn from_has_settings_write(has_settings_write: bool) -> Self {
        if has_settings_write {
            Self::Bypass
        } else {
            Self::Enforce
        }
    }
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

/// System-wide retention policy for locally-built deployment images.
///
/// The nightly cleanup removes a Temps-built image only once *every*
/// deployment that references it is older than the owning project's retention
/// window. Deleting an image makes rollback/promotion to that deployment
/// impossible, so the default is deliberately generous: it is a rollback
/// window, not a cache TTL.
#[derive(Debug, Clone, Serialize, ToSchema, Deserialize)]
#[serde(default)]
pub struct ImageRetentionSettings {
    /// Whether the nightly pass removes expired deployment images at all.
    /// Disabling it keeps every built image forever (the pre-0.1 behaviour).
    pub enabled: bool,

    /// Default hours to keep a built deployment image when the owning project
    /// has no `image_retention_hours` override. Valid range 1..=8760.
    #[schema(minimum = 1, maximum = 8760, example = 336)]
    pub default_hours: i64,
}

impl Default for ImageRetentionSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            // 14 days. Long enough that a rollback is still possible after a
            // quiet week; short enough to bound disk growth. A 48h default
            // would silently destroy the rollback history of any project that
            // did not deploy over a long weekend.
            default_hours: 336,
        }
    }
}

impl ImageRetentionSettings {
    /// Clamp `default_hours` into the range the projects API accepts, so a
    /// hand-edited settings row can never produce a cutoff that deletes images
    /// the moment they are built (or one that never expires by accident).
    pub fn effective_default_hours(&self) -> i64 {
        self.default_hours.clamp(1, 8760)
    }
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
    /// Default max agent turns for the autofixer *analysis* phase when this
    /// provider runs it. `None` = built-in default (10). Only enforced for
    /// CLIs that support a turn cap (Claude Code's `--max-turns`); Codex and
    /// OpenCode run to completion regardless.
    #[serde(default)]
    pub max_turns_analysis: Option<i32>,
    /// Default max agent turns for the autofixer *fix* phase.
    /// `None` = built-in default (20).
    #[serde(default)]
    pub max_turns_fix: Option<i32>,
    /// Default max agent turns for autofixer *feedback/re-analyze* rounds.
    /// `None` = built-in default (10).
    #[serde(default)]
    pub max_turns_feedback: Option<i32>,
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
    /// Default isolation backend for sandboxes: "docker" (default) or
    /// "firecracker" (ADR-029; requires `temps firecracker setup`). Only
    /// consulted when the Firecracker backend probes available — otherwise
    /// Docker is used regardless.
    #[serde(default)]
    #[schema(example = "docker")]
    pub sandbox_backend: Option<String>,
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
            sandbox_backend: None,
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
                max_turns_analysis: None,
                max_turns_fix: None,
                max_turns_feedback: None,
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
    /// Docker image reference for the gateway. Pinned by digest per Temps release.
    /// Operators can override this to test a custom build.
    #[schema(
        example = "ghcr.io/gotempsh/temps-preview-gateway@sha256:a16d4346f2f857470fdd28c9ed46809f6db4f7e577888d6250338f8d5dcf04b9"
    )]
    pub image: String,
    /// Host port to publish the gateway on (always bound to 127.0.0.1).
    /// Pingora forwards `ws-*` traffic to this port after authenticating.
    #[schema(example = 8090)]
    pub host_port: u16,
    /// Docker container name for this instance's gateway.
    ///
    /// A single Temps install owns the whole host, so the default is fine and
    /// operators never need to touch this. It exists for the case where
    /// several Temps instances share one Docker daemon — most obviously a
    /// development machine with multiple checkouts running at once.
    ///
    /// Without it those instances silently fight: the `shared_secret` is
    /// per-database, so each generates a different one, but they all
    /// reconcile the *same* container name. Each start-up sees the other's
    /// container as drifted, recreates it with its own secret, and every
    /// other instance's previews start failing with "missing or invalid
    /// X-Temps-Preview-Token". Giving each instance its own container name
    /// (and `host_port`) makes them independent.
    #[serde(default = "default_preview_gateway_container")]
    #[schema(example = "temps-preview-gateway")]
    pub container_name: String,
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

/// Serde default for [`PreviewGatewaySettings::container_name`], so a settings
/// row written before this field existed deserialises to today's name rather
/// than to an empty string (which would mean "container named ''").
fn default_preview_gateway_container() -> String {
    "temps-preview-gateway".to_string()
}

impl Default for PreviewGatewaySettings {
    fn default() -> Self {
        Self {
            image: "ghcr.io/gotempsh/temps-preview-gateway@sha256:a16d4346f2f857470fdd28c9ed46809f6db4f7e577888d6250338f8d5dcf04b9".to_string(),
            host_port: 8090,
            container_name: default_preview_gateway_container(),
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
    /// Optional: ClickHouse cluster. The runtime store is built from the
    /// `TEMPS_CLICKHOUSE_*` server env configuration; selecting this without
    /// that configuration falls back to TimescaleDB (reported via
    /// `effective_metrics_store`).
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
    #[schema(minimum = 1, maximum = 10, example = 2)]
    pub retention_daily_years: u32,

    /// ClickHouse DSN (legacy, optional). The runtime metrics store is built
    /// from the `TEMPS_CLICKHOUSE_*` env vars, never from this field; it is
    /// retained for compatibility and operator reference only.
    /// Example: `"http://localhost:8123"`.
    pub clickhouse_url: Option<String>,
}

/// TimescaleDB compression policy configuration for append-only observability
/// tables. Values are expressed in hours so operators can choose sub-day
/// windows while keeping the API representation unambiguous.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct ObservabilityCompressionSettings {
    /// Compress proxy-log chunks after this many hours. Defaults to 24 hours.
    #[schema(minimum = 1, maximum = 720, example = 24)]
    pub proxy_logs_after_hours: u32,

    /// Compress OpenTelemetry span chunks after this many hours. Defaults to
    /// 24 hours.
    #[schema(minimum = 1, maximum = 2160, example = 24)]
    pub otel_spans_after_hours: u32,
}

impl Default for ObservabilityCompressionSettings {
    fn default() -> Self {
        Self {
            proxy_logs_after_hours: 24,
            otel_spans_after_hours: 24,
        }
    }
}

/// Retention policy configuration for raw observability tables. Values are in
/// days. The Settings API applies them to TimescaleDB; ClickHouse-backed proxy
/// logs and spans retain their storage-level per-row TTL behavior.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct ObservabilityRetentionSettings {
    /// Retain proxy request logs for this many days.
    #[schema(minimum = 1, maximum = 3650, example = 30)]
    pub proxy_logs_days: u32,

    /// Retain OpenTelemetry spans (traces) for this many days.
    #[schema(minimum = 1, maximum = 3650, example = 90)]
    pub otel_spans_days: u32,

    /// Retain OpenTelemetry log events for this many days.
    #[schema(minimum = 1, maximum = 3650, example = 90)]
    pub otel_logs_days: u32,

    /// Retain OpenTelemetry metric points for this many days.
    #[schema(minimum = 1, maximum = 3650, example = 90)]
    pub otel_metrics_days: u32,
}

impl Default for ObservabilityRetentionSettings {
    fn default() -> Self {
        Self {
            proxy_logs_days: 30,
            otel_spans_days: 90,
            otel_logs_days: 90,
            otel_metrics_days: 90,
        }
    }
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
            edge_target: None,
            console_force_https: None,
            screenshots: ScreenshotSettings::default(),
            letsencrypt: LetsEncryptSettings::default(),
            dns_provider: DnsProviderSettings::default(),
            security_headers: SecurityHeadersSettings::default(),
            rate_limiting: RateLimitSettings::default(),
            docker_registry: DockerRegistrySettings::default(),
            registry_mirror_prefix: None,
            image_retention: ImageRetentionSettings::default(),
            disk_space_alert: DiskSpaceAlertSettings::default(),
            container_logs: ContainerLogSettings::default(),
            multi_node: MultiNodeSettings::default(),
            agent_sandbox: AgentSandboxSettings::default(),
            preview_gateway: PreviewGatewaySettings::default(),
            on_demand_tls: OnDemandTlsSettings::default(),
            ai_config: AiConfigSettings::default(),
            insecure_tls: false,
            ai_chat_limits: AiChatLimitsSettings::default(),
            request_timeouts: RequestTimeoutSettings::default(),
            connection_limits: ConnectionLimitSettings::default(),
            tenant_resource_ceilings: TenantResourceCeilings::default(),
            build_limits: BuildLimitsSettings::default(),
            cluster_dns: ClusterDnsSettings::default(),
            monitoring: MonitoringSettings::default(),
            observability_compression: ObservabilityCompressionSettings::default(),
            observability_retention: ObservabilityRetentionSettings::default(),
            mcp_server: McpServerSettings::default(),
            setup_complete: false,
            require_mfa_for_admins: false,
            self_update: None,
            console_version: None,
        }
    }
}

impl AppSettings {
    /// Effective self-update settings, treating "never configured" as the
    /// default. Use this everywhere instead of touching the `Option` directly,
    /// so absence and an explicit default behave identically at read time.
    pub fn self_update(&self) -> SelfUpdateSettings {
        self.self_update.clone().unwrap_or_default()
    }
}

/// Controls the console's one-click "Update now" action.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(default)]
pub struct SelfUpdateSettings {
    /// Allow admins to apply a release and restart the server from the console.
    /// `true` by default: the action is permission-gated, audited, and only
    /// ever installs an official release whose published SHA-256 matches.
    ///
    /// Turning this off hides nothing — the console still shows the update
    /// banner and the manual command, it just refuses to run it for you.
    #[schema(example = true)]
    pub enabled: bool,

    /// Release channel this install tracks: `stable`, `beta` or `nightly`.
    ///
    /// `None` (the default) means "infer from the running version tag", which
    /// is what the CLI has always done — a `-nightly.` build tracks nightly, a
    /// `-beta.N` build tracks beta, a plain tag tracks stable. Setting it
    /// explicitly pins the channel, so an operator can move a nightly box back
    /// onto stable without reinstalling.
    #[schema(example = "stable")]
    pub channel: Option<String>,
}

impl Default for SelfUpdateSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            channel: None,
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

    /// Serialize into an EXISTING `settings.data` document, preserving
    /// top-level keys this struct does not own.
    ///
    /// The singleton `settings` row is a shared JSON document. `AppSettings`
    /// owns most of it, but other subsystems store their own sub-documents on
    /// the same row under their own key — today `admin_gate` (written by
    /// `AdminGateService`), and anything added later. Those keys are invisible
    /// to serde here: `from_json` drops them and `to_json` never re-emits them,
    /// so writing `to_json()` straight over `data` DELETES them.
    ///
    /// That is not theoretical — the console records `console_version` through
    /// `update_setting_field` on every startup, which wiped the operator's
    /// admin-gate allowlist (IPs + Host headers) before the gate had even been
    /// loaded, silently reverting the management surface to "open to any host"
    /// on each restart. Every settings write must go through this method so a
    /// subsystem's sub-document survives an unrelated save.
    ///
    /// Keys this struct owns always win, so a field can still be updated back
    /// to its default value.
    pub fn to_json_merged(&self, existing: &serde_json::Value) -> serde_json::Value {
        let incoming = self.to_json();
        let (Some(existing_map), serde_json::Value::Object(incoming_map)) =
            (existing.as_object(), incoming)
        else {
            // Existing blob isn't an object (fresh row, or corrupt), or we
            // somehow didn't serialize to one: nothing to preserve, so the
            // serialized settings are the whole document.
            return self.to_json();
        };

        // `incoming_map` is owned, so move the values in rather than cloning
        // every key and every serialized sub-document.
        let mut merged = existing_map.clone();
        merged.extend(incoming_map);
        serde_json::Value::Object(merged)
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

    /// Hostname the Temps console is served on, derived from `external_url`.
    ///
    /// Returns `None` when `external_url` is unset or unparsable (installs
    /// reached by raw IP), in which case there is no console hostname to
    /// protect.
    pub fn console_hostname(&self) -> Option<String> {
        let raw = self.external_url.as_ref()?.trim();
        if raw.is_empty() {
            return None;
        }
        // Tolerate a bare host ("console.example.com") as well as a full URL.
        let candidate = if raw.contains("://") {
            raw.to_string()
        } else {
            format!("https://{raw}")
        };
        url::Url::parse(&candidate)
            .ok()?
            .host_str()
            .map(|h| h.trim_end_matches('.').to_ascii_lowercase())
    }

    /// True when `host` is owned by the platform itself and must never be
    /// claimed by a project domain.
    ///
    /// Reserved hosts are the console hostname (`external_url`) and the
    /// preview domain apex — routing either of them at a project makes the
    /// console or every generated preview URL unreachable, and recovering
    /// requires shell/IP access to the box (issue #478).
    pub fn is_reserved_hostname(&self, host: &str) -> bool {
        let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
        if host.is_empty() {
            return false;
        }
        if self.console_hostname().as_deref() == Some(host.as_str()) {
            return true;
        }
        let preview = self
            .preview_domain
            .trim()
            .trim_end_matches('.')
            .to_ascii_lowercase();
        !preview.is_empty() && preview == host
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Issue #478: a project domain must never be allowed to claim the
    // console hostname — doing so locks the operator out of the console and
    // recovery requires the raw public IP.
    #[test]
    fn console_hostname_parses_url_and_bare_host() {
        let with_external_url = |raw: Option<&str>| AppSettings {
            external_url: raw.map(str::to_string),
            ..Default::default()
        };

        assert_eq!(
            with_external_url(Some("https://Console.Example.com:8443/"))
                .console_hostname()
                .as_deref(),
            Some("console.example.com")
        );
        assert_eq!(
            with_external_url(Some("console.example.com"))
                .console_hostname()
                .as_deref(),
            Some("console.example.com")
        );
        assert_eq!(with_external_url(Some("   ")).console_hostname(), None);
        assert_eq!(with_external_url(None).console_hostname(), None);
    }

    #[test]
    fn reserved_hostname_covers_console_and_preview_apex() {
        let s = AppSettings {
            external_url: Some("https://console.example.com".to_string()),
            preview_domain: "apps.example.com".to_string(),
            ..Default::default()
        };

        assert!(s.is_reserved_hostname("console.example.com"));
        // Case and trailing-dot variants are the same host.
        assert!(s.is_reserved_hostname("CONSOLE.example.com."));
        assert!(s.is_reserved_hostname("apps.example.com"));

        // Ordinary project domains, including subdomains of the preview
        // domain, stay assignable.
        assert!(!s.is_reserved_hostname("shop.example.com"));
        assert!(!s.is_reserved_hostname("my-app.apps.example.com"));
        assert!(!s.is_reserved_hostname(""));
    }

    #[test]
    fn reserved_hostname_is_inert_without_external_url() {
        let s = AppSettings {
            external_url: None,
            preview_domain: String::new(),
            ..Default::default()
        };
        assert!(!s.is_reserved_hostname("anything.example.com"));
    }

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
    fn legacy_settings_json_uses_observability_retention_defaults() {
        let parsed = AppSettings::from_json(serde_json::json!({
            "external_url": "https://paas.example.com",
            "preview_domain": "localho.st"
        }));

        assert_eq!(
            parsed.observability_retention,
            ObservabilityRetentionSettings::default()
        );
        assert_eq!(parsed.observability_retention.proxy_logs_days, 30);
        assert_eq!(parsed.observability_retention.otel_spans_days, 90);
    }

    #[test]
    fn observability_retention_round_trips_through_json() {
        let mut settings = AppSettings::default();
        settings.observability_retention.proxy_logs_days = 14;
        settings.observability_retention.otel_spans_days = 60;
        settings.observability_retention.otel_logs_days = 45;
        settings.observability_retention.otel_metrics_days = 30;

        let parsed = AppSettings::from_json(settings.to_json());

        assert_eq!(
            parsed.observability_retention,
            settings.observability_retention
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

    #[test]
    fn observability_compression_defaults_to_24_hours() {
        let compression = ObservabilityCompressionSettings::default();
        assert_eq!(compression.proxy_logs_after_hours, 24);
        assert_eq!(compression.otel_spans_after_hours, 24);
    }

    #[test]
    fn legacy_settings_get_24_hour_observability_compression_defaults() {
        let parsed = AppSettings::from_json(serde_json::json!({
            "external_url": "https://paas.example.com",
            "preview_domain": "localho.st"
        }));

        assert_eq!(parsed.observability_compression.proxy_logs_after_hours, 24);
        assert_eq!(parsed.observability_compression.otel_spans_after_hours, 24);
    }

    #[test]
    fn observability_compression_round_trips_through_json() {
        let mut settings = AppSettings::default();
        settings.observability_compression.proxy_logs_after_hours = 12;
        settings.observability_compression.otel_spans_after_hours = 48;

        let parsed = AppSettings::from_json(settings.to_json());
        assert_eq!(parsed.observability_compression.proxy_logs_after_hours, 12);
        assert_eq!(parsed.observability_compression.otel_spans_after_hours, 48);
    }

    #[test]
    fn request_timeouts_default_is_no_timeout_opt_in_only() {
        // No traffic-class default applies a timeout out of the box — an
        // existing app with a slow endpoint or long-lived connection must
        // keep working exactly as it did before this setting existed.
        // Timeouts are opt-in: an operator sets a nonzero global default
        // and/or a project/environment sets its own override. The ceiling
        // stays at a sane value because it only ever constrains a timeout
        // that's actually configured — it can't create one on its own.
        let s = RequestTimeoutSettings::default();
        assert_eq!(s.max_request_timeout_seconds, 600);
        assert_eq!(s.default_http_timeout_seconds, 0);
        assert_eq!(s.default_sse_idle_timeout_seconds, 0);
        assert_eq!(s.default_websocket_idle_timeout_seconds, 0);
    }

    #[test]
    fn request_timeouts_ceiling_clamps_out_of_range_values() {
        let mut s = RequestTimeoutSettings {
            max_request_timeout_seconds: 0,
            ..RequestTimeoutSettings::default()
        };
        assert_eq!(s.ceiling(), RequestTimeoutSettings::MIN_CEILING_SECS);

        s.max_request_timeout_seconds = u32::MAX;
        assert_eq!(s.ceiling(), RequestTimeoutSettings::MAX_CEILING_SECS);
    }

    #[test]
    fn request_timeouts_clamp_to_ceiling_never_exceeds_ceiling() {
        let s = RequestTimeoutSettings {
            max_request_timeout_seconds: 120,
            ..RequestTimeoutSettings::default()
        };
        assert_eq!(s.clamp_to_ceiling(30), 30, "below ceiling: pass through");
        assert_eq!(s.clamp_to_ceiling(120), 120, "at ceiling: pass through");
        assert_eq!(s.clamp_to_ceiling(9000), 120, "above ceiling: clamped");
    }

    #[test]
    fn legacy_settings_json_without_request_timeouts_deserializes() {
        // An old `settings.data` row written before this feature shipped has
        // no `request_timeouts` key. `#[serde(default)]` must fill it in
        // with the no-timeout defaults so pre-migration rows keep loading
        // with identical (i.e. unbounded) proxy behavior.
        let legacy = serde_json::json!({
            "external_url": "https://paas.example.com",
            "preview_domain": "localho.st"
        });
        let parsed = AppSettings::from_json(legacy);
        assert_eq!(parsed.request_timeouts, RequestTimeoutSettings::default());
    }

    #[test]
    fn request_timeouts_round_trip_through_json() {
        let mut settings = AppSettings::default();
        settings.request_timeouts.max_request_timeout_seconds = 120;
        settings.request_timeouts.default_http_timeout_seconds = 30;
        settings.request_timeouts.default_sse_idle_timeout_seconds = 90;
        settings
            .request_timeouts
            .default_websocket_idle_timeout_seconds = 90;

        let parsed = AppSettings::from_json(settings.to_json());
        assert_eq!(parsed.request_timeouts, settings.request_timeouts);
    }

    #[test]
    fn connection_limits_default_is_unlimited_opt_in_only() {
        // Out of the box, no cap is applied — an existing app with many
        // concurrent requests must keep working without any operator action.
        // The limit is opt-in: an operator sets a nonzero global default
        // and/or a project/environment sets its own override.
        let s = ConnectionLimitSettings::default();
        assert_eq!(s.default_max_concurrent_connections, 0);
    }

    #[test]
    fn connection_limits_round_trip_through_json() {
        let mut settings = AppSettings::default();
        settings
            .connection_limits
            .default_max_concurrent_connections = 200;

        let parsed = AppSettings::from_json(settings.to_json());
        assert_eq!(
            parsed.connection_limits.default_max_concurrent_connections,
            200
        );
    }

    #[test]
    fn legacy_settings_json_without_connection_limits_deserializes() {
        // An old `settings.data` row written before this feature shipped has
        // no `connection_limits` key. `#[serde(default)]` must fill it in
        // with the unlimited default so pre-existing deployments aren't
        // suddenly capped on upgrade.
        let legacy = serde_json::json!({
            "external_url": "https://paas.example.com",
            "preview_domain": "localho.st"
        });
        let parsed = AppSettings::from_json(legacy);
        assert_eq!(parsed.connection_limits, ConnectionLimitSettings::default());
        assert_eq!(
            parsed.connection_limits.default_max_concurrent_connections,
            0
        );
    }

    /// Regression: a settings save must not delete the `admin_gate`
    /// sub-document. The console writes `console_version` through
    /// `update_setting_field` on every startup; with a plain `to_json()`
    /// overwrite that wiped the operator's admin allowlist and reopened the
    /// management surface to every host on each restart.
    #[test]
    fn merge_preserves_foreign_admin_gate_subdocument() {
        let existing = serde_json::json!({
            "preview_domain": "temps.kfs.es",
            "admin_gate": {
                "allowed_ips": ["10.0.0.0/8"],
                "allowed_hosts": ["app.temps.kfs.es"],
                "trust_forwarded_for": false
            }
        });

        let mut settings = AppSettings::from_json(existing.clone());
        settings.console_version = Some("v0.1.0".to_string());

        let merged = settings.to_json_merged(&existing);

        assert_eq!(
            merged.get("admin_gate"),
            existing.get("admin_gate"),
            "an unrelated settings write must not drop the admin_gate sub-document"
        );
        assert_eq!(
            merged.get("console_version").and_then(|v| v.as_str()),
            Some("v0.1.0"),
        );
        assert_eq!(
            merged.get("preview_domain").and_then(|v| v.as_str()),
            Some("temps.kfs.es"),
        );
    }

    /// Keys `AppSettings` owns must still be updatable — including back to
    /// their default value — so the merge cannot simply prefer the stored blob.
    #[test]
    fn merge_lets_owned_fields_win_over_stored_values() {
        let existing = serde_json::json!({
            "preview_domain": "old.example.com",
            "insecure_tls": true,
            "admin_gate": { "allowed_hosts": ["app.example.com"] }
        });

        let mut settings = AppSettings::from_json(existing.clone());
        settings.preview_domain = "new.example.com".to_string();
        settings.insecure_tls = false;

        let merged = settings.to_json_merged(&existing);

        assert_eq!(
            merged.get("preview_domain").and_then(|v| v.as_str()),
            Some("new.example.com"),
        );
        assert_eq!(
            merged.get("insecure_tls").and_then(|v| v.as_bool()),
            Some(false),
            "a field must be settable back to its default value"
        );
        assert!(merged.get("admin_gate").is_some());
    }

    /// A fresh/corrupt row has nothing to preserve — the serialized settings
    /// become the whole document.
    #[test]
    fn merge_falls_back_to_plain_serialization_for_non_object_blob() {
        let settings = AppSettings::default();
        let merged = settings.to_json_merged(&serde_json::Value::Null);
        assert_eq!(merged, settings.to_json());
    }
}
