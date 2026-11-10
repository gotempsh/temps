// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use temps_core::{
    ClickHouseCompression, EncryptionService, ObservabilityRetentionSettings,
    MAX_ANALYTICS_RETENTION_DAYS, MIN_ANALYTICS_RETENTION_DAYS,
};
use utoipa::ToSchema;

mod env_migration;
mod otel_backup;
#[cfg(test)]
mod tests;

pub use env_migration::{
    decide_env_setting_migration, decide_secret_env_migration, record_env_setting_persisted,
    EnvMigrationLedger, EnvSecret, EnvSettingMigratedAudit, EnvSettingMigration,
    ENV_MIGRATION_LEDGER_KEY,
};
pub use otel_backup::{
    BackupExecutorSettings, OtelIngestSettings, OtelIngestSettingsError, OtelS3ArchiveSettings,
    OtelS3Credentials, DEFAULT_BACKUP_EXECUTOR_MAX_CONCURRENT, DEFAULT_OTEL_RATE_LIMIT_REQUESTS,
    DEFAULT_OTEL_RATE_LIMIT_WINDOW_SECS, DEFAULT_OTEL_S3_ARCHIVE_PREFIX,
    MAX_BACKUP_EXECUTOR_MAX_CONCURRENT,
};

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
    /// Base URL handed to third parties that call back into Temps: email
    /// open/click tracking, git provider webhooks, SES/SNS events, revenue
    /// webhooks, the Sentry DSN (ADR-051). `None` (the default) falls back to
    /// `external_url`, which is right whenever the console is publicly
    /// reachable. Set it when the console host is restricted (VPN, firewall,
    /// identity-aware proxy) so only this hostname has to be public: the proxy
    /// serves nothing but the public callback routes on it. Read it through
    /// [`AppSettings::effective_public_ingress_url`].
    #[serde(default)]
    pub public_ingress_url: Option<String>,
    pub preview_domain: String,
    /// Label of a new environment's generated hostname under `preview_domain`,
    /// from `{project}` and `{env}` (ADR 058 D9). `None` means
    /// [`temps_core::public_hostname::DEFAULT_ENVIRONMENT_LABEL_TEMPLATE`]. Applies to
    /// environments created after it is set; existing ones keep their names.
    #[serde(default)]
    pub environment_hostname_template: Option<String>,
    /// Label for an environment named `production`, when it should differ
    /// (e.g. `{project}` for `app.example.com`). `None` uses
    /// `environment_hostname_template`.
    #[serde(default)]
    pub production_hostname_template: Option<String>,
    /// Public edge target that generated DNS records point at when a managed
    /// domain opts into automatic record sync. An IPv4/IPv6 address produces an
    /// `A`/`AAAA` record; anything else is treated as a `CNAME` target. `None`
    /// disables DNS record sync regardless of per-domain opt-in.
    pub edge_target: Option<String>,
    /// Enable Cloudflare delivery by default for projects created after this is set.
    pub cloudflare_new_projects: bool,
    /// Enable Bunny delivery by default only for projects created after this is set.
    pub bunny_new_projects: bool,

    /// Managed control-plane connection. Credentials are deliberately not
    /// stored here; they live in the owner-only cloud-link state file.
    pub cloud: CloudSettings,

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
    /// Allow the proxy to use forwarding headers from a loopback peer for
    /// client IP attribution. `None` means an older client did not send the
    /// field; the settings handler preserves the stored decision on PUT.
    /// This is the sole control surface — there is no CLI/env override.
    #[serde(default)]
    pub trust_loopback_forwarded_ip: Option<bool>,
    /// Opt in to platform identity in proxy responses. Missing values preserve the stored choice.
    #[serde(default)]
    pub expose_platform_identity: Option<bool>,
    /// The header a trusted loopback proxy puts the visitor's JA4 TLS
    /// fingerprint in (ADR 066, D4), e.g. `x-ja4`. Read only from a peer
    /// `trust_loopback_forwarded_ip` trusts, and removed from every request
    /// before it goes upstream. `None` records no JA4 behind such a proxy.
    /// Set through [`normalize_loopback_ja4_header`].
    #[serde(default)]
    pub loopback_ja4_header: Option<String>,

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

    /// Workspace preview gateway settings (single shared container per node).
    /// Owned by `PATCH /preview-gateway/settings` and
    /// `POST /preview-gateway/upgrade`, which change the gateway's containers
    /// to match; the generic settings update preserves the stored value.
    pub preview_gateway: PreviewGatewaySettings,

    /// Managed KV service (Redis container) settings. Owned by
    /// `PATCH /kv/settings`; the generic settings update preserves the stored
    /// value.
    pub kv: KvSettings,

    /// Managed Blob service (RustFS container) settings. Owned by
    /// `PATCH /blob/settings`; the generic settings update preserves the
    /// stored value.
    pub blob: BlobSettings,

    // On-demand (lazy) HTTP-01 TLS issuance settings (ADR-018). On for new
    // installations (see `AppSettings::default`); a stored row that predates the
    // section stays off via `legacy_on_demand_tls_settings`, so an upgrade never
    // starts issuing certificates the operator was not told about.
    #[serde(default = "legacy_on_demand_tls_settings")]
    pub on_demand_tls: OnDemandTlsSettings,

    // AI configuration settings (global config repo for skills, MCP servers, etc.)
    pub ai_config: AiConfigSettings,

    /// Limits on a single AI chat turn. Operator-tunable because the right
    /// value depends on the model: a turn against a slow self-hosted model can
    /// legitimately take ten minutes, while a hosted one finishes in seconds
    /// and a shorter ceiling keeps costs predictable.
    #[serde(default)]
    pub ai_chat_limits: AiChatLimitsSettings,

    /// Transfer and preview limits for files in persistent AI workspaces.
    /// These are runtime settings because operators have different control
    /// plane memory budgets and commonly work with very different asset sizes.
    #[serde(default)]
    pub ai_workspace_file_limits: AiWorkspaceFileLimitsSettings,

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

    /// Private networks (CIDRs or single addresses) that user-configured
    /// outbound requests may reach: webhooks, notification channels, uptime
    /// checks, AI gateway providers, self-hosted git, importers. Empty by
    /// default, which keeps the SSRF guard blocking every private, loopback
    /// and CGNAT destination. Only private ranges are accepted, and cloud
    /// metadata endpoints stay blocked regardless. Published process-wide via
    /// `url_validation::set_trusted_private_networks`.
    #[serde(default)]
    pub trusted_private_networks: Vec<String>,

    /// Build-time resource limits applied on the control plane to prevent
    /// `docker build` from saturating host CPU/RAM. Worker nodes are
    /// intentionally NOT subject to these limits (each worker is dedicated
    /// hardware that already has its own per-host headroom).
    pub build_limits: BuildLimitsSettings,

    /// The control plane's (node 0's) scheduling settings, the counterpart
    /// of `nodes.resource_policy` / `nodes.system_reserved` on worker rows
    /// (ADR-055 §5). Stored here because node 0 is not a `nodes` row; changed
    /// through `PUT /nodes/0/resources`.
    #[serde(default)]
    pub control_plane_resources: ControlPlaneResourceSettings,

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

    /// Retention windows for product analytics data and for the personal
    /// identifiers (visitors, client IP addresses) attached to it.
    /// Applied by a background job, not at save time, so a change takes
    /// effect within the job's interval.
    #[serde(default)]
    pub analytics_retention: AnalyticsRetentionSettings,

    /// How Temps talks to the instance's ClickHouse server (wire compression
    /// of query responses). Read once when the process starts, so a change
    /// is applied by the next restart; the Settings API reports the mode the
    /// running process actually uses next to the stored one.
    pub clickhouse: ClickHouseSettings,

    /// Geolocation database refresh policy, MaxMind credential (encrypted at
    /// rest), and the self-recorded freshness metadata of the last refresh.
    #[serde(default)]
    pub geo: GeoSettings,

    /// OTLP ingest rate limits, storage quota, background analysis jobs and
    /// S3 log archival (credentials encrypted at rest). Replaces the
    /// `TEMPS_OTEL_*` environment variables; applied at the next restart.
    #[serde(default)]
    pub otel_ingest: OtelIngestSettings,

    /// Backup executor concurrency. Replaces
    /// `TEMPS_BACKUP_EXECUTOR_MAX_CONCURRENT`; applied at the next restart.
    #[serde(default)]
    pub backup_executor: BackupExecutorSettings,

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

    /// Share verified external-plugin installation counts with the official
    /// registry. Defaults to off; each plugin receives an unlinkable ID.
    #[serde(default)]
    pub plugin_installation_reporting_enabled: bool,

    /// Admin preference for anonymous product telemetry (the events sent to
    /// the Temps maintainers by `temps-product-telemetry`; unrelated to Temps Cloud
    /// mirroring in `cloud.telemetry_enabled` and to OpenTelemetry ingest).
    ///
    /// - `None` (default) — the operator has not chosen; the built-in default
    ///   applies (see `temps_product_telemetry::DEFAULT_TELEMETRY_ENABLED`).
    /// - `Some(true)` / `Some(false)` — an admin turned it on/off from
    ///   Settings › Telemetry. Applied at runtime without a restart.
    ///
    /// The `TEMPS_TELEMETRY=0` environment variable is a host-level kill
    /// switch that wins over this value unconditionally. The dedicated
    /// `PATCH /settings/telemetry` endpoint is the only write path: the
    /// generic settings save restores the stored value under the row lock, so
    /// an older client round-tripping the whole document cannot flip it.
    #[serde(default)]
    pub anonymous_telemetry_enabled: Option<bool>,

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

    /// ADR 066: platform-wide Shield settings.
    #[serde(default)]
    pub shield: ShieldSettings,

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

/// Non-secret managed control-plane settings stored with application settings.
///
/// `PartialEq` but deliberately **not** `Eq`: `telemetry_bulk_anomaly_factor` is
/// a float, and the total-equality contract `Eq` promises is one `f32` cannot
/// keep. Nothing compares two `CloudSettings` for equality outside this module's
/// own tests, so the weaker bound costs nothing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct CloudSettings {
    /// HTTPS origin used for enrollment and telemetry mirroring.
    pub backend_url: String,
    /// Explicit consent to mirror locally stored telemetry.
    pub telemetry_enabled: bool,
    /// Explicit consent to export completed backup objects.
    pub backups_enabled: bool,
    /// Explicit consent to send notifications through managed providers.
    pub notifications_enabled: bool,

    /// Explicit operator consent to let Temps Cloud open this instance's
    /// console over the outbound relay, using managed OIDC authentication.
    /// Default off for every enrollment path. Linking, including unattended
    /// bootstrap, never enables console access; enable it explicitly in
    /// Settings > Temps Cloud or with `temps cloud console-access enable`.
    #[serde(default)]
    pub console_access_enabled: bool,

    /// Set once Temps Cloud's bootstrap file has applied its telemetry-export
    /// consent (ADR-045 Amendment 2). Cloud drops that file on every upgrade
    /// of a hosted plane; this makes the consent apply exactly once, so an
    /// operator who later turns telemetry off is not overridden by the next
    /// upgrade. Never set on a self-hosted instance.
    #[serde(default)]
    pub telemetry_consent_bootstrapped: bool,

    /// ADR-041 §3d: hard ceiling, in bytes, on the durable span outbox that
    /// backs Cloud-primary telemetry writes.
    ///
    /// An operator setting on the singleton `settings` row rather than an
    /// environment variable, per CLAUDE.md, so it can be raised at runtime by
    /// the operator watching a queue fill up — which is exactly when they need
    /// to change it and exactly when restarting the binary is the worst
    /// available option.
    ///
    /// Expressed in **bytes and not rows**: the reference deployment is
    /// 3 vCPU / 4 GB, and a row count says nothing about disk on a table whose
    /// rows are serialized spans of wildly varying size. When the queue reaches
    /// this size the instance stops accepting new spans for Cloud-primary
    /// projects and records a gap window with a start, an end and a count —
    /// see [`DEFAULT_CLOUD_TELEMETRY_OUTBOX_MAX_BYTES`] for how the default is
    /// sized and what it buys.
    #[serde(default = "default_cloud_telemetry_outbox_max_bytes")]
    #[schema(minimum = 1048576, example = 536870912)]
    pub telemetry_outbox_max_bytes: u64,

    /// ADR-042 §3: optional throttle, in spans per second, on a **bulk Cloud
    /// telemetry activation** backfill.
    ///
    /// `None` — the default — is unthrottled, which is what "activate now"
    /// means and is right for an instance that is idle or being cut over
    /// deliberately. An operator running an activation against a live instance
    /// can set a ceiling so the backfill stops competing with their own read IO
    /// and with the Cloud ingest allowance.
    ///
    /// An operator setting on the singleton `settings` row rather than an
    /// environment variable, per CLAUDE.md, so it can be changed **while a job
    /// is running** — which is exactly when an operator discovers they need it,
    /// and exactly when restarting the binary would mean stopping an activation
    /// they have already paid for. The worker re-reads it each time it picks up
    /// a project, so a change takes effect at the next project boundary rather
    /// than at the next restart.
    ///
    /// Only the bulk worker reads this. The live Cloud-primary write path is a
    /// primary path and is never throttled; the offline
    /// `temps backfill cloud-telemetry` tool keeps its own
    /// `--rate-limit-spans-per-sec` flag, because it runs in a different
    /// process with the server stopped.
    #[serde(default)]
    #[schema(minimum = 1, example = 5000)]
    pub telemetry_bulk_rate_limit_spans_per_sec: Option<u32>,

    /// ADR-042 §6.3: how far a project's shipped bytes may exceed its pre-send
    /// estimate before the bulk activation stops that project.
    ///
    /// `None` — the default — resolves to
    /// [`DEFAULT_CLOUD_TELEMETRY_BULK_ANOMALY_FACTOR`]. The guard exists because
    /// an estimate that is wrong by an order of magnitude means a bug, and *a
    /// bug that costs money should stop* rather than run away with a customer's
    /// egress spend on a path that has no human confirm behind it.
    ///
    /// An operator setting on the singleton `settings` row rather than an
    /// environment variable, per CLAUDE.md: the right multiple depends on how
    /// heterogeneous that instance's spans actually are, which only the operator
    /// running it can know, and they must be able to widen it — or narrow it —
    /// without restarting the binary mid-activation.
    ///
    /// Below `1.0` every project would pause on its first chunk, and above
    /// [`MAX_CLOUD_TELEMETRY_BULK_ANOMALY_FACTOR`] the guard stops being a
    /// tuning knob and becomes an off switch. The settings write path rejects
    /// values outside that range, and the effective value is clamped on read
    /// besides; see [`CloudSettings::effective_bulk_anomaly_factor`].
    ///
    /// The schema bounds below are documentation for a client; they are not
    /// what enforces this. The server validates the range on write.
    #[serde(default)]
    #[schema(minimum = 1.0, maximum = 50.0, example = 5.0)]
    pub telemetry_bulk_anomaly_factor: Option<f32>,
}

/// Default durable-outbox ceiling: 512 MiB.
///
/// Sized so that a completely full queue is a small fraction of the reference
/// deployment's disk and cannot fill it, while still buying a long outage. At
/// the `Queryable` projection's typical few-hundred bytes per span, 512 MiB is
/// on the order of a million spans — roughly a day at 12 spans/second, or about
/// eight hours at the 35 spans/second the Phase B1 load test sustains.
///
/// Deliberately generous rather than minimal: the cost of an over-large cap is
/// disk an operator can see and change, and the cost of an under-sized one is
/// telemetry that no longer exists.
pub const DEFAULT_CLOUD_TELEMETRY_OUTBOX_MAX_BYTES: u64 = 512 * 1024 * 1024;

/// Smallest accepted ceiling: 1 MiB.
///
/// A cap below one batch's worth of spans would make the queue drop
/// continuously while reporting itself as merely "full", so the settings write
/// path refuses it rather than accepting a value that cannot work.
pub const MIN_CLOUD_TELEMETRY_OUTBOX_MAX_BYTES: u64 = 1024 * 1024;

fn default_cloud_telemetry_outbox_max_bytes() -> u64 {
    DEFAULT_CLOUD_TELEMETRY_OUTBOX_MAX_BYTES
}

/// Default byte-budget anomaly factor for a bulk Cloud activation: **5×**.
///
/// # This number is a placeholder, and says so
///
/// ADR-042 Open Question 1 asks what multiple of the estimate should pause a
/// project and answers: *"Needs a number from real backfill data, not a guess."*
/// No such data exists yet, so rather than hard-code a constant that pretends to
/// be authoritative this is the *default* of a setting an operator can change —
/// and the number itself is chosen to be defensible from what is known about how
/// the estimate is produced:
///
/// - `estimate_backfill` extrapolates the whole window from the **first**
///   `ESTIMATE_SAMPLE_SIZE` (1,000) spans of it, at that project's projection
///   fidelity. Spans in one project are not uniform — an error span carrying a
///   stack trace, or a request with many allowlisted attributes, serializes
///   several times larger than a bare health-check span — so the mean over the
///   head of a window can legitimately understate the mean over all of it by a
///   small multiple. A 2× budget would pause a large amount of perfectly valid
///   work.
/// - ADR-042 §6.3 says the condition worth stopping for is an estimate *"wrong
///   by an order of magnitude"*. 5× sits below one order of magnitude, so a
///   genuine 10×+ over-run still trips it, while ordinary sampling skew does
///   not.
/// - The failure modes are asymmetric. Too tight costs an operator a retry
///   click on a project that stopped early with its cursor intact; too loose
///   costs a customer money that cannot be given back. When in doubt, stop.
pub const DEFAULT_CLOUD_TELEMETRY_BULK_ANOMALY_FACTOR: f32 = 5.0;

/// Smallest accepted anomaly factor: `1.0`.
///
/// A factor below 1 means "pause before the estimate is even reached", which
/// would stop every project on its first chunk and make a paid-for activation
/// impossible to complete. Clamped rather than rejected so a hand-written `0`
/// degrades to "budget equals the estimate" instead of bricking activation.
pub const MIN_CLOUD_TELEMETRY_BULK_ANOMALY_FACTOR: f32 = 1.0;

/// Largest accepted anomaly factor: `50.0`.
///
/// The setting exists so an operator can **tune** the guard to how heterogeneous
/// their spans actually are. It does not exist so the guard can be switched off,
/// and without a ceiling that is exactly what it becomes: one field on the
/// settings document, set to `1e12`, silently converts the purchase path — the
/// path that spends a customer's money with no human confirm — back into an
/// unbounded one, and nothing on any screen says so.
///
/// `50.0` is ten times the default and an order of magnitude past the ADR's own
/// threshold for "this is a bug, stop" (§6.3: an estimate *"wrong by an order of
/// magnitude"*). An instance whose real span-size skew exceeds 50× is not one
/// this guard can usefully bound — the honest answer there is the operator path,
/// where the estimate is shown and confirmed before anything ships, not a wider
/// blind budget.
///
/// Enforced in two places on purpose, and they are not redundant: the settings
/// write path **rejects** an out-of-range value with a 400 so the operator finds
/// out immediately, and [`CloudSettings::effective_bulk_anomaly_factor`] clamps
/// on read so a row written by an older build, by hand, or by a restore cannot
/// reach the worker with a value the current build would refuse.
pub const MAX_CLOUD_TELEMETRY_BULK_ANOMALY_FACTOR: f32 = 50.0;

impl Default for CloudSettings {
    fn default() -> Self {
        Self {
            backend_url: "https://app.temps.sh".to_string(),
            telemetry_enabled: false,
            backups_enabled: false,
            notifications_enabled: false,
            // Linking never changes this explicit operator consent.
            console_access_enabled: false,
            telemetry_consent_bootstrapped: false,
            telemetry_outbox_max_bytes: DEFAULT_CLOUD_TELEMETRY_OUTBOX_MAX_BYTES,
            // ADR-042 §3: unthrottled by default. "Activate now" is what the
            // customer paid for, and a throttle nobody asked for makes a long
            // activation longer for no stated reason.
            telemetry_bulk_rate_limit_spans_per_sec: None,
            // ADR-042 §6.3 / Open Question 1: `None` resolves to the documented
            // default rather than to "no guard at all". The purchase path spends
            // money with no human confirm, so the guard must be on by default —
            // an operator opting *into* a safety net they do not know exists is
            // not a safety net.
            telemetry_bulk_anomaly_factor: None,
        }
    }
}

impl CloudSettings {
    /// The effective outbox ceiling, clamped to something that can actually
    /// work.
    ///
    /// A settings row written by an older build has no value at all (serde
    /// supplies the default); a row written by hand could carry `0`, which
    /// would mean "drop every span and record a permanent gap". Both resolve to
    /// a usable number here rather than at each of the several call sites that
    /// would otherwise have to remember.
    pub fn effective_outbox_max_bytes(&self) -> u64 {
        self.telemetry_outbox_max_bytes
            .max(MIN_CLOUD_TELEMETRY_OUTBOX_MAX_BYTES)
    }

    /// The effective bulk-activation throttle, with `Some(0)` resolved.
    ///
    /// A row written by hand could carry `0`, which read literally means "ship
    /// zero spans per second" — a job that never finishes while reporting
    /// itself as running. Treated as "no throttle" here rather than at the call
    /// site, so the unusable value cannot reach the worker.
    pub fn effective_bulk_rate_limit_spans_per_sec(&self) -> Option<u32> {
        self.telemetry_bulk_rate_limit_spans_per_sec
            .filter(|per_second| *per_second > 0)
    }

    /// The effective byte-budget anomaly factor, always a usable number.
    ///
    /// Unlike the throttle, `None` here does **not** mean "off": an unset value
    /// is a settings row written by a build that predates the guard, and
    /// resolving that to "no budget" would silently disable a money guard on
    /// every existing instance the moment they upgrade. A hand-written `0`,
    /// negative or non-finite value is clamped to
    /// [`MIN_CLOUD_TELEMETRY_BULK_ANOMALY_FACTOR`] for the same reason the
    /// outbox cap has a floor: an unusable value must never reach the worker.
    ///
    /// Clamped at the **top** as well, to
    /// [`MAX_CLOUD_TELEMETRY_BULK_ANOMALY_FACTOR`]. A factor with no ceiling is
    /// not a tuning knob, it is a way to switch the money guard off from a
    /// single settings field — and unlike the floor, a value above the ceiling
    /// fails in the direction that costs a customer money rather than a retry
    /// click. The write path rejects such a value outright so the operator hears
    /// about their mistake; this clamp is what protects a row that was written
    /// before the ceiling existed, edited by hand, or restored from a backup.
    pub fn effective_bulk_anomaly_factor(&self) -> f32 {
        match self.telemetry_bulk_anomaly_factor {
            Some(factor) if factor.is_finite() => factor.clamp(
                MIN_CLOUD_TELEMETRY_BULK_ANOMALY_FACTOR,
                MAX_CLOUD_TELEMETRY_BULK_ANOMALY_FACTOR,
            ),
            // Unset, or NaN/infinity from a hand-edited row.
            _ => DEFAULT_CLOUD_TELEMETRY_BULK_ANOMALY_FACTOR,
        }
    }
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
    /// Whether an operator has ever changed `enabled` themselves. Until then,
    /// the control plane turns cluster DNS on when the first worker joins,
    /// since cross-node links need `*.temps.local`. Set by the server when a
    /// save changes `enabled`; a client cannot clear it.
    #[schema(example = false)]
    pub operator_chosen: bool,
}

impl ClusterDnsSettings {
    /// The settings to store after a save that asked for `requested`, given
    /// what was stored before. Changing `enabled` is an operator choice, and
    /// once made it stays made, whatever the client sent for the marker.
    pub fn after_operator_save(stored: &Self, requested: &Self) -> Self {
        Self {
            enabled: requested.enabled,
            operator_chosen: stored.operator_chosen || requested.enabled != stored.enabled,
        }
    }

    /// Whether a worker joining should turn cluster DNS on: only while it is
    /// off and no operator has decided either way.
    pub fn should_enable_for_worker(&self) -> bool {
        !self.enabled && !self.operator_chosen
    }
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

/// Runtime limits for workspace file transfer and browser previews.
///
/// The HTTP layer additionally enforces absolute ceilings so a malformed or
/// legacy settings row cannot turn a configurable limit into unbounded control
/// plane memory use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct AiWorkspaceFileLimitsSettings {
    #[schema(minimum = 1, maximum = 100, example = 32)]
    pub max_files_per_upload: u32,
    #[schema(minimum = 1, maximum = 32, example = 16)]
    pub max_file_size_mb: u32,
    #[schema(minimum = 1, maximum = 32, example = 32)]
    pub max_upload_size_mb: u32,
    #[schema(minimum = 1, maximum = 2048, example = 256)]
    pub max_workspace_size_mb: u32,
    #[schema(minimum = 1, maximum = 50000, example = 5000)]
    pub max_workspace_entries: u32,
    #[schema(minimum = 1, maximum = 1024, example = 256)]
    pub max_text_preview_kb: u32,
    #[schema(minimum = 1, maximum = 16, example = 8)]
    pub max_image_preview_size_mb: u32,
    #[schema(minimum = 1, maximum = 32, example = 32)]
    pub max_download_size_mb: u32,
}

impl Default for AiWorkspaceFileLimitsSettings {
    fn default() -> Self {
        Self {
            max_files_per_upload: 32,
            max_file_size_mb: 16,
            max_upload_size_mb: 32,
            max_workspace_size_mb: 256,
            max_workspace_entries: 5_000,
            max_text_preview_kb: 256,
            max_image_preview_size_mb: 8,
            max_download_size_mb: 32,
        }
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
/// is allowed to consume. A single global gate in the deployer crate
/// admits at most `max_concurrent` `DockerRuntime::build_image` calls at a
/// time. When the gate is full, additional builds queue and wait — they do
/// not fail. Per-build CPU/memory caps are forwarded to Docker via
/// `BuildImageOptions { memory, cpuquota, cpuperiod }`.
///
/// Changes apply live, with no restart. Builds already running finish with the
/// limits they started with; queued and new builds use the new ones. Lowering
/// `max_concurrent` never stops a running build: new builds wait until enough
/// of them finish to get under the new limit.
///
/// `cpu_limit_cores = 0.0` or `memory_limit_mb = 0` means "no explicit cap"
/// — fall back to the legacy 50%-of-host heuristic for backwards
/// compatibility with operators who never visit the settings page.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct BuildLimitsSettings {
    /// Maximum number of `docker build` operations allowed to run at the
    /// same time on the control plane. Additional builds queue. Min 1.
    /// Applies live: raising it starts queued builds right away; lowering it
    /// lets running builds finish and holds new ones until they do.
    #[schema(minimum = 1, example = 2)]
    pub max_concurrent: u32,

    /// CPU cores allowed per build (float, e.g. 2.0 = 2 cores, 0.5 = half
    /// a core). 0 means "use the legacy 50%-of-host default". Applied only
    /// by Docker's legacy builder: BuildKit, the default since Docker 18.09,
    /// ignores the CPU and memory options of the image build API, so on
    /// BuildKit hosts this has no effect. Applies live to builds that start
    /// after the change; a running build keeps the cap it started with.
    #[schema(minimum = 0.0, example = 2.0)]
    pub cpu_limit_cores: f32,

    /// Memory allowed per build, in megabytes. 0 means "use the legacy
    /// 50%-of-host default". Same scope as `cpu_limit_cores`: applied only
    /// by the legacy builder, ignored by BuildKit, applied live to builds that
    /// start after the change.
    /// Values above 2047 MB are reduced to 2047 MB, the most the build API
    /// accepts through the client, with a warning in the server log.
    #[schema(minimum = 0, example = 2048)]
    pub memory_limit_mb: u32,

    /// Upper bound for Docker's build cache, in gigabytes. The nightly
    /// cleanup removes the least recently used cache records that no build
    /// is using until the total is at or below this size. 0 disables the
    /// size cap (only `build_cache_max_unused_days` applies). Re-read on
    /// every nightly run. Applies to this host's Docker daemon; worker nodes
    /// use the default.
    #[schema(minimum = 0, example = 10)]
    pub build_cache_max_gb: u32,

    /// Build cache records unused for longer than this many days are removed
    /// by the nightly cleanup, whatever the total size. Valid range 1..=365.
    #[schema(minimum = 1, maximum = 365, example = 7)]
    pub build_cache_max_unused_days: u32,
}

impl BuildLimitsSettings {
    /// Default build cache cap, in gigabytes.
    pub const DEFAULT_BUILD_CACHE_MAX_GB: u32 = 10;
    /// Default age after which unused build cache is removed, in days.
    pub const DEFAULT_BUILD_CACHE_MAX_UNUSED_DAYS: u32 = 7;

    /// The build cache size cap in bytes, or `None` when the cap is disabled.
    pub fn build_cache_max_bytes(&self) -> Option<u64> {
        match self.build_cache_max_gb {
            0 => None,
            gb => Some(u64::from(gb) * 1024 * 1024 * 1024),
        }
    }

    /// `build_cache_max_unused_days` clamped into 1..=365, so a hand-edited
    /// settings row can neither wipe the cache every night (0) nor keep it
    /// for decades.
    pub fn effective_build_cache_max_unused_days(&self) -> u32 {
        self.build_cache_max_unused_days.clamp(1, 365)
    }
}

/// The control plane's scheduling settings (ADR-055 §5).
///
/// `None` reserves take the §5 default for the control plane, which depends
/// on whether its PostgreSQL runs on the same host (0.5 vCPU / 1 GB) or not
/// (0.25 vCPU / 512 MB). Those defaults are estimates.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct ControlPlaneResourceSettings {
    /// `true` = `enforced`: refuse a placement on the control plane whose
    /// reservations exceed its capacity minus the reserve. `false` (the
    /// default) = `best_effort`.
    pub enforce_reservations: bool,
    /// CPU kept for Temps itself, in millicores. `None` = the default.
    pub system_reserved_cpu_millis: Option<u32>,
    /// Memory kept for Temps itself, in megabytes. `None` = the default.
    pub system_reserved_memory_mb: Option<u32>,
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
            build_cache_max_gb: Self::DEFAULT_BUILD_CACHE_MAX_GB,
            build_cache_max_unused_days: Self::DEFAULT_BUILD_CACHE_MAX_UNUSED_DAYS,
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
    /// Disk budget, in MiB, for the collected-log read cache (`logs/cache`
    /// under the data dir): recently read chunk blocks, block indexes and
    /// bloom filters kept locally so searches over object storage do not
    /// re-fetch them (ADR-046 §6). Applied within a minute of saving;
    /// shrinking evicts immediately.
    #[schema(minimum = 64, maximum = 1048576, example = 2048)]
    pub cache_mb: u32,
    /// Per-container cap, in MiB, on unsealed log lines held in memory (and
    /// the WAL) before they are sealed into a chunk object. Larger buffers
    /// mean fewer, bigger chunks; smaller ones bound memory per container.
    #[schema(minimum = 1, maximum = 256, example = 8)]
    pub head_buffer_mb: u32,
    /// Per-container ingestion budget, in lines per second. A container
    /// over budget has its excess lines dropped (not stored or indexed),
    /// a notice written into its log, and is listed as throttled in the log
    /// explorer. Up to 10 seconds of budget may arrive at once. `0` disables
    /// the line limit. Applied within a minute of saving. A project or
    /// managed service can override it (`log_ingest_lines_per_sec`).
    #[schema(minimum = 0, maximum = 1000000, example = 250)]
    pub ingest_lines_per_sec: u32,
    /// Per-container ingestion budget, in KiB per second, enforced alongside
    /// `ingest_lines_per_sec`. `0` disables the byte limit. A project or
    /// managed service can override it (`log_ingest_kib_per_sec`).
    #[schema(minimum = 0, maximum = 1048576, example = 256)]
    pub ingest_kib_per_sec: u32,
}

impl ContainerLogSettings {
    pub const MAX_INGEST_LINES_PER_SEC: u32 = 1_000_000;
    pub const MAX_INGEST_KIB_PER_SEC: u32 = 1_048_576;

    /// Check a per-project / per-service override (`log_ingest_lines_per_sec`,
    /// `log_ingest_kib_per_sec`) against the same range as the platform
    /// budget. `None` (inherit) is always valid; `0` means unlimited.
    pub fn validate_ingest_override(
        lines_per_sec: Option<i32>,
        kib_per_sec: Option<i32>,
    ) -> Result<(), String> {
        let check = |field: &str, value: Option<i32>, max: u32| {
            match value {
            Some(v) if v < 0 || v as u32 > max => Err(format!(
                "{field} {v} is not in valid range (0-{max}; 0 = unlimited, null = platform default)"
            )),
            _ => Ok(()),
        }
        };
        check(
            "log_ingest_lines_per_sec",
            lines_per_sec,
            Self::MAX_INGEST_LINES_PER_SEC,
        )?;
        check(
            "log_ingest_kib_per_sec",
            kib_per_sec,
            Self::MAX_INGEST_KIB_PER_SEC,
        )
    }
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
    ///   - `opencode`:  "config_file" | "openai_compatible"
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
    /// Default isolation backend for sandboxes: "docker" (default),
    /// "firecracker" (ADR-029; requires `temps firecracker setup`) or
    /// "microsandbox" (ADR-050, experimental; requires `temps microsandbox
    /// setup`). Only consulted when the selected microVM backend probes
    /// available — otherwise Docker is used.
    #[serde(default)]
    #[schema(example = "docker")]
    pub sandbox_backend: Option<String>,
    /// Nodes allowed to run sandboxes (ADR-048). `None` (the default) =
    /// every node, including the control plane. `Some(ids)` = only those
    /// nodes; the control plane is id `0`. `Some([])` disables sandbox
    /// creation. Owned by `PUT /sandboxes/placement`; the generic settings
    /// update preserves the stored value.
    #[serde(default)]
    pub allowed_node_ids: Option<Vec<i32>>,
    /// Highest `cpu_limit` (cores) a `POST /sandboxes` request may ask for.
    /// `cpu_limit` above only sets the default for requests that omit the
    /// field; this is the ceiling. `None` = the built-in ceiling (16 cores).
    /// A value above the built-in ceiling has no effect.
    #[serde(default)]
    #[schema(example = 4.0, minimum = 0.01)]
    pub max_cpu_limit: Option<f64>,
    /// Highest `memory_limit_mb` a `POST /sandboxes` request may ask for.
    /// `None` = the built-in ceiling (32768 MB). On the control plane the
    /// host's physical memory is an additional ceiling.
    #[serde(default)]
    #[schema(example = 8192, minimum = 6)]
    pub max_memory_limit_mb: Option<u64>,
    /// Highest `pids_limit` a `POST /sandboxes` request may ask for.
    /// `None` = the built-in ceiling (4096).
    #[serde(default)]
    #[schema(example = 1024, minimum = 1)]
    pub max_pids_limit: Option<i64>,
    /// Most sandboxes (not destroyed) one user may have created through
    /// `POST /sandboxes` at a time; a create over the cap is refused.
    /// `None` = unlimited. AI application workspaces, agent runs and the
    /// sandboxes the platform creates for AI chat or autofix do not count
    /// and are not refused. Sandboxes created before this setting existed
    /// are not counted either.
    #[serde(default)]
    #[schema(example = 10, minimum = 1)]
    pub max_sandboxes_per_user: Option<u32>,
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
            allowed_node_ids: None,
            max_cpu_limit: None,
            max_memory_limit_mb: None,
            max_pids_limit: None,
            max_sandboxes_per_user: None,
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
    /// `ip:port` workers dial for the control plane's end of the WireGuard
    /// mesh, published instead of `private_address` on the mesh port. For a
    /// control plane reachable only through NAT or a forwarded port.
    ///
    /// Owned by `temps serve --wg-endpoint` / `TEMPS_WG_ENDPOINT`: every start
    /// records the flag's value (clearing it when the flag is absent), and a
    /// settings write never changes it. Stored here only so the processes
    /// that set the mesh up (the deployer plugin, `temps network
    /// setup-multi-node`) read the same value.
    #[serde(default)]
    pub wg_endpoint: Option<String>,
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
    /// Whether to enforce multi-node mTLS (ADR-020 WS-2.1). New installations
    /// default to `true`. Existing serialized settings that predate this field
    /// deserialize it as `false`, providing an explicit migration window rather
    /// than unexpectedly disconnecting legacy workers. When `true`, the CP signs
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
    /// Seconds a worker node must go without a heartbeat before its workloads
    /// are failed over to healthy nodes. A node is reported offline (and
    /// operators alerted) well before this; the gap is a grace period so a
    /// brief network partition or a control-plane stall does not redeploy a
    /// whole node's worth of apps that never stopped serving. `None` disables
    /// automatic failover entirely. Default 300.
    #[serde(default = "default_node_failover_after_secs")]
    pub node_failover_after_secs: Option<u64>,
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
fn default_node_failover_after_secs() -> Option<u64> {
    Some(300)
}

fn default_legacy_shared_token_enabled() -> bool {
    true
}

impl Default for MultiNodeSettings {
    fn default() -> Self {
        Self {
            join_token_hash: None,
            private_address: None,
            wg_endpoint: None,
            legacy_shared_token_enabled: true,
            cluster_ca_cert_pem: None,
            cluster_ca_key_encrypted: None,
            require_mtls: true,
            node_cpu_alert_percent: default_node_cpu_alert_percent(),
            node_memory_alert_percent: default_node_memory_alert_percent(),
            node_disk_alert_percent: default_node_disk_alert_percent(),
            node_failover_after_secs: default_node_failover_after_secs(),
        }
    }
}

/// Workspace preview gateway settings.
///
/// The preview gateway uses a private routing container plus a hardened ingress
/// relay bound to host loopback. The router joins each sandbox's isolated
/// network and routes requests to workspace dev servers based on the `Host`
/// header (`ws-<sid>-<port>.<preview_domain>`), while the relay never joins a
/// tenant network. `temps serve` reconciles both containers on startup; these
/// settings let an operator override the router image, host port, and
/// auto-upgrade behavior.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct PreviewGatewaySettings {
    /// Whether Temps runs the shared preview gateway. While false its
    /// containers are removed, so workspace preview URLs are not served.
    #[schema(example = true)]
    pub enabled: bool,
    /// Docker image reference. Empty follows this Temps release's digest;
    /// a nonempty value is an explicit operator pin.
    #[schema(
        example = "ghcr.io/gotempsh/temps-preview-gateway@sha256:02d5cdd382c3285d569032e84321d5ce8fc089372a3f08651119f6eda8cb1448"
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
            enabled: true,
            image: String::new(),
            host_port: 8090,
            container_name: default_preview_gateway_container(),
            auto_upgrade: true,
            shared_secret: String::new(),
        }
    }
}

/// Settings of the managed KV service (the Redis container behind `/kv`).
///
/// The Docker container is named `redis-<service name>`, so two Temps
/// instances sharing one Docker daemon collide unless each has its own name.
/// An unset value follows the per-instance default (the pre-existing
/// `redis-temps-kv` for an install that already enabled KV, a name derived
/// from this instance otherwise).
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct KvSettings {
    /// Docker container name of the KV Redis container. Empty means the
    /// per-instance default. A custom value must start with `redis-`.
    #[schema(example = "redis-temps-kv-staging")]
    pub container_name: String,
}

/// Settings of the managed Blob service (the RustFS container behind `/blob`).
///
/// Same reasoning as [`KvSettings`]: the container is named
/// `rustfs-<service name>`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct BlobSettings {
    /// Docker container name of the Blob RustFS container. Empty means the
    /// per-instance default. A custom value must start with `rustfs-`.
    #[schema(example = "rustfs-temps-blob-staging")]
    pub container_name: String,
}

/// On-demand (lazy) HTTP-01 TLS issuance settings (ADR-018).
///
/// When `enabled`, the proxy's `certificate_callback` triggers ACME HTTP-01
/// issuance for allowlisted, STABLE hostnames (per-environment aliases and the
/// console host) that have no active cert, rather than silently failing the
/// handshake. Ephemeral per-deployment hostnames are NEVER certed (ADR §2).
///
/// New installations start with it on (`AppSettings::default`); the zone comes
/// from `zone` or is derived from `external_url`/`preview_domain`, and nothing
/// is issued while none can be derived or `external_url` is loopback. A settings
/// row stored before this section existed stays off until the operator enables
/// it.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct OnDemandTlsSettings {
    /// Master switch. When `false` the proxy's on-demand cert gate rejects
    /// every SNI and no issuance is ever triggered. New installations start
    /// with it `true`; a stored settings row that predates this section keeps
    /// `false`.
    #[schema(example = true)]
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

/// The value for a stored settings row that has no (or an unreadable)
/// `on_demand_tls` section: feature off, exactly what such an install ran with
/// before the section existed.
fn legacy_on_demand_tls_settings() -> OnDemandTlsSettings {
    OnDemandTlsSettings::default()
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

/// Longest accepted `monitoring.scrape_interval_secs`.
///
/// Windowed metrics are computed from the deltas between scrapes: the
/// Postgres cache hit ratio covers the last 15 minutes
/// (`temps_metrics::cache_hit_window::CACHE_HIT_WINDOW`) and only counts
/// scrape intervals that lie entirely inside that window. Five minutes keeps
/// at least two whole intervals in it. A longer interval would leave the
/// window empty on every scrape, so the ratio and its alarm would silently
/// stop.
pub const MAX_SCRAPE_INTERVAL_SECS: u64 = 300;

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
    /// Saving accepts 15 to 300 s: windowed metrics such as the Postgres
    /// cache hit ratio need several scrapes in their 15 minutes. A value
    /// above 300 s stored before that limit is kept
    /// when a save leaves it unchanged, and the MetricsScraper clamps it to
    /// 300 s at runtime, as it clamps anything below 10 s to 10 s.
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

/// Why a [`AppSettings::loopback_ja4_header`] value was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LoopbackJa4HeaderError {
    #[error("'{0}' is not a valid HTTP header name")]
    InvalidName(String),
    /// The proxy strips this header from every request, so a name without
    /// `ja4` in it could strip one an app needs, such as `cookie`.
    #[error("'{0}' must contain \"ja4\", for example x-ja4")]
    NotJa4(String),
}

/// Trim and lowercase a [`AppSettings::loopback_ja4_header`]; blank means
/// none. The name must be an HTTP token (RFC 9110, 5.6.2) that contains
/// `ja4`.
pub fn normalize_loopback_ja4_header(
    value: Option<&str>,
) -> Result<Option<String>, LoopbackJa4HeaderError> {
    let Some(name) = value.map(str::trim).filter(|name| !name.is_empty()) else {
        return Ok(None);
    };
    let is_token_char = |c: char| c.is_ascii_alphanumeric() || "!#$%&'*+-.^_`|~".contains(c);
    if !name.chars().all(is_token_char) {
        return Err(LoopbackJa4HeaderError::InvalidName(name.to_string()));
    }
    let name = name.to_ascii_lowercase();
    if !name.contains("ja4") {
        return Err(LoopbackJa4HeaderError::NotJa4(name));
    }
    Ok(Some(name))
}

/// Settings for the connection to the instance's ClickHouse server.
///
/// The connection itself (URL, database, user, password) is bootstrap
/// configuration in `TEMPS_CLICKHOUSE_*`; only behaviour an operator may need
/// to change at runtime lives here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema, Default)]
#[serde(default)]
pub struct ClickHouseSettings {
    /// Wire compression for query responses. See [`ClickHouseCompression`].
    pub compression: ClickHouseCompression,
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

/// Default for [`AnalyticsRetentionSettings::events_days`]. Retention
/// reconciliation also updates the historical schema's shorter policy.
pub const DEFAULT_ANALYTICS_EVENTS_RETENTION_DAYS: u32 = 365;
/// Default for [`AnalyticsRetentionSettings::unreferenced_ip_days`].
pub const DEFAULT_UNREFERENCED_IP_RETENTION_DAYS: u32 = 30;

/// Days added to the proxy-log window before a stored IP address may be
/// deleted. An address's `last_used_at` is refreshed at most every 12 hours
/// and a cached lookup reuses its id for 6 more, so the last log that points
/// at an address can be written up to ~18 hours after the timestamp the
/// sweep reads. A whole day covers that lag and the delay before the proxy
/// applies a newly saved window.
pub const IP_REFERENCE_SLACK_DAYS: u32 = 1;

/// How long product analytics data, and the identifiers attached to it, are
/// kept.
///
/// `ip_geolocations` holds the full client IP address and is referenced by
/// events, proxy logs, error events, visitors, performance metrics and audit
/// logs. Before these settings existed nothing ever removed an IP once stored,
/// and nothing removed visitors, so the retention of the referencing tables
/// said nothing about how long an address was kept.
///
/// A job removes an IP row only when **no** table references it, so the
/// effective retention of an address is the longest window among the data that
/// still points at it. Audit logs keep their IP for as long as the audit
/// record exists.
///
/// `None` on an optional window means "keep" -- the behaviour before the
/// setting existed. Events have a required window (365 days by default),
/// including the ClickHouse copy on upgrade. The unreferenced-IP cleanup
/// defaults to 30 days and only drops cache rows that nothing points at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct AnalyticsRetentionSettings {
    /// Retain analytics events (page views, custom events) for this many
    /// days. Applied to the TimescaleDB events policy and, when configured,
    /// the ClickHouse events TTL. Reconciled hourly and after ClickHouse
    /// migrations. ClickHouse expiry follows asynchronous background merges.
    #[schema(minimum = 1, maximum = 3650, example = 365)]
    pub events_days: u32,

    /// Retain captured error events for this many days. `null` keeps them
    /// until the project is deleted. Error groups keep their counters.
    #[schema(minimum = 1, maximum = 3650, nullable = true, example = 90)]
    pub error_events_days: Option<u32>,

    /// Delete visitors that have not been seen for this many days. Their
    /// session replays are deleted with them. With ClickHouse enabled, their
    /// project-scoped events are deleted from both databases; failures leave
    /// visitors for retry. With PostgreSQL only, events are detached and kept.
    /// Proxy logs and error events keep their own retention. This is not
    /// erasure of all visitor data. `null` keeps visitors indefinitely.
    #[schema(minimum = 1, maximum = 3650, nullable = true, example = 425)]
    pub visitor_days: Option<u32>,

    /// Delete a stored client IP address (and its derived location) once
    /// nothing references it and it has not been looked up for this many
    /// days. Never applied sooner than the proxy-log retention
    /// (`observability_retention.proxy_logs_days`): request logs kept outside
    /// PostgreSQL still point at the address. `null` keeps every address ever
    /// seen.
    #[schema(minimum = 1, maximum = 3650, nullable = true, example = 30)]
    pub unreferenced_ip_days: Option<u32>,

    /// Server-managed; a value sent by a client is replaced on save. Set when
    /// `observability_retention.proxy_logs_days` is lowered: request logs
    /// stored outside PostgreSQL keep the window they were written with, so
    /// they can still point at an address for the previous window. Stored IP
    /// addresses are not deleted until this moment has passed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(ignore)]
    pub ip_hold_until: Option<DateTime<Utc>>,
}

impl Default for AnalyticsRetentionSettings {
    fn default() -> Self {
        Self {
            events_days: DEFAULT_ANALYTICS_EVENTS_RETENTION_DAYS,
            error_events_days: None,
            visitor_days: None,
            unreferenced_ip_days: Some(DEFAULT_UNREFERENCED_IP_RETENTION_DAYS),
            ip_hold_until: None,
        }
    }
}

impl AnalyticsRetentionSettings {
    /// Recompute [`Self::ip_hold_until`] for a save that moves the proxy-log
    /// retention from `previous_days` to `next_days`, starting from the value
    /// that was stored (a client-supplied one is never trusted).
    ///
    /// Lowering the window means logs already written still live for up to
    /// `previous_days`, so addresses are held until then; a later lowering can
    /// only extend the hold. Raising or keeping it changes nothing, and a hold
    /// that has passed is dropped.
    pub fn with_ip_hold(
        mut self,
        stored_hold: Option<DateTime<Utc>>,
        previous_days: u32,
        next_days: u32,
        now: DateTime<Utc>,
    ) -> Self {
        let mut hold = stored_hold.filter(|until| *until > now);
        if next_days < previous_days {
            let until = now
                + chrono::Duration::days(
                    i64::from(previous_days) + i64::from(IP_REFERENCE_SLACK_DAYS),
                );
            hold = Some(hold.map_or(until, |existing| existing.max(until)));
        }
        self.ip_hold_until = hold;
        self
    }

    /// The first window outside `1..=3650`, as `(field, value)`.
    pub fn out_of_range(&self) -> Option<(&'static str, u32)> {
        let windows = [
            ("events_days", Some(self.events_days)),
            ("error_events_days", self.error_events_days),
            ("visitor_days", self.visitor_days),
            ("unreferenced_ip_days", self.unreferenced_ip_days),
        ];
        windows.into_iter().find_map(|(field, days)| {
            let days = days?;
            (!(MIN_ANALYTICS_RETENTION_DAYS..=MAX_ANALYTICS_RETENTION_DAYS).contains(&days))
                .then_some((field, days))
        })
    }
}

/// How often the scheduled job re-downloads the GeoLite2 city database when the
/// admin has not chosen an interval. MaxMind publishes GeoLite2 twice a week,
/// so a daily check picks up a new build within a day of release.
pub const DEFAULT_GEO_REFRESH_INTERVAL_HOURS: u32 = 24;
/// Lower bound on the refresh interval. Guards against a `0` turning the job
/// into a download loop that would get the operator's license key rate-limited.
pub const MIN_GEO_REFRESH_INTERVAL_HOURS: u32 = 1;
/// Upper bound (one year). Past this the value is almost certainly a units
/// mistake (days or minutes typed as hours).
pub const MAX_GEO_REFRESH_INTERVAL_HOURS: u32 = 24 * 365;

/// How old a cached IP -> location row may get before the next lookup
/// re-resolves it against the in-memory database.
pub const DEFAULT_GEO_STALE_LOOKUP_DAYS: u32 = 30;
pub const MIN_GEO_STALE_LOOKUP_DAYS: u32 = 1;
pub const MAX_GEO_STALE_LOOKUP_DAYS: u32 = 365 * 10;

/// `GeoSettings::source` when the database came from MaxMind's authenticated
/// endpoint using the operator's license key.
pub const GEO_SOURCE_MAXMIND_OFFICIAL: &str = "maxmind_official";
/// `GeoSettings::source` when the database came from the copy committed to the
/// Temps repository (no license key needed).
pub const GEO_SOURCE_BUNDLED_GITHUB: &str = "bundled_github";
/// `GeoSettings::last_check_status` for a refresh attempt that succeeded.
pub const GEO_CHECK_STATUS_OK: &str = "ok";
/// `GeoSettings::last_check_status` for a refresh attempt that failed.
pub const GEO_CHECK_STATUS_ERROR: &str = "error";
/// `GeoSettings::last_check_status` when the scheduled job deliberately did not
/// download anything because no MaxMind license key is configured.
///
/// Recorded rather than left silent: an operator who sees "no refresh in 40
/// days" has to be able to tell a broken download from a database that is not
/// being refreshed by design, and the status endpoint, `temps doctor` and the
/// settings UI all render this as "add a license key to enable refreshes".
pub const GEO_CHECK_STATUS_SKIPPED_NO_LICENSE_KEY: &str = "skipped_no_license_key";

/// Whether a settings write intended to change the stored MaxMind license key.
///
/// The plaintext key is encrypted (and consumed) by the handler *before* the
/// service takes the settings row's write lock, so by the time the row is
/// locked the incoming ciphertext is indistinguishable from a value carried
/// forward out of a stale snapshot. Threading the intent explicitly is what
/// lets the service tell "this request set a key" from "this request happened
/// to be built from a snapshot that had one", and therefore keep a
/// concurrently-saved key instead of reverting it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum GeoLicenseKeyIntent {
    /// The request did not touch the key: whatever is on the locked row wins.
    #[default]
    Unchanged,
    /// The request submitted a new key, already encrypted into
    /// [`GeoSettings::maxmind_license_key_encrypted`].
    Set,
    /// The request explicitly cleared the key.
    Cleared,
}

/// What went wrong handling the encrypted MaxMind license key.
///
/// Neither variant's `reason` can carry key material: both are built from
/// `EncryptionService` failures, which report cipher/encoding problems and
/// never echo their input.
#[derive(Debug, thiserror::Error)]
pub enum GeoSettingsError {
    #[error("Failed to encrypt the MaxMind license key before storing it: {reason}")]
    EncryptLicenseKey { reason: String },

    #[error(
        "Failed to decrypt the stored MaxMind license key (it may have been encrypted with a \
         different server encryption key; re-enter it in Settings): {reason}"
    )]
    DecryptLicenseKey { reason: String },
}

/// Geolocation database configuration and freshness state.
///
/// Both the data-policy knobs an admin sets and the metadata the refresh job
/// records live on one typed struct on purpose. The `settings` row is a shared
/// JSON document and `AppSettings` is deserialized/reserialized in full by the
/// generic settings endpoint, so any geo key kept *outside* this struct would
/// be silently dropped the next time an unrelated settings page was saved.
///
/// The license key is stored as ciphertext only
/// ([`GeoSettings::maxmind_license_key_encrypted`]). The plaintext field
/// beside it is write-only input from the admin UI: it is `skip_serializing`,
/// so it can never be persisted or returned, and
/// [`GeoSettings::apply_license_key_update`] clears it after encrypting.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct GeoSettings {
    /// How often the scheduled refresh job runs. `None` means
    /// [`DEFAULT_GEO_REFRESH_INTERVAL_HOURS`]; read it through
    /// [`GeoSettings::effective_refresh_interval_hours`].
    #[schema(minimum = 1, maximum = 8760, example = 24)]
    pub refresh_interval_hours: Option<u32>,

    /// Age at which a stored IP -> location row is re-resolved on its next
    /// lookup. `None` means [`DEFAULT_GEO_STALE_LOOKUP_DAYS`]; read it through
    /// [`GeoSettings::effective_stale_lookup_days`].
    #[schema(minimum = 1, maximum = 3650, example = 30)]
    pub stale_lookup_days: Option<u32>,

    /// Plaintext MaxMind license key, accepted on a settings write only.
    ///
    /// `skip_serializing` is load-bearing: this field is never persisted to
    /// the settings row and never appears in any response, so a plaintext key
    /// cannot leak through a GET-then-PUT round trip even if a caller forgets
    /// to run [`GeoSettings::apply_license_key_update`].
    ///
    /// Blank or absent preserves the stored key, matching the email-provider
    /// credential convention; use [`GeoSettings::clear_maxmind_license_key`]
    /// to actually remove it.
    #[serde(default, skip_serializing)]
    pub maxmind_license_key: Option<String>,

    /// Remove the stored license key, reverting downloads to the bundled
    /// repository copy. Write-only, like the plaintext field above.
    #[serde(default, skip_serializing)]
    pub clear_maxmind_license_key: bool,

    /// AES-256-GCM ciphertext of the MaxMind license key, as produced by
    /// `EncryptionService::encrypt_string`. Never returned by the API.
    pub maxmind_license_key_encrypted: Option<String>,

    /// When new database bytes were last installed and swapped in.
    /// Self-recorded by the refresh job; never writable by a client.
    #[schema(value_type = Option<String>, format = DateTime)]
    pub last_refreshed_at: Option<DateTime<Utc>>,

    /// [`GEO_SOURCE_MAXMIND_OFFICIAL`] or [`GEO_SOURCE_BUNDLED_GITHUB`].
    pub source: Option<String>,

    /// MaxMind `build_epoch` of the database that was last installed.
    pub build_epoch: Option<u64>,

    /// When a refresh was last attempted, successful or not.
    #[schema(value_type = Option<String>, format = DateTime)]
    pub last_check_at: Option<DateTime<Utc>>,

    /// [`GEO_CHECK_STATUS_OK`] or [`GEO_CHECK_STATUS_ERROR`].
    pub last_check_status: Option<String>,

    /// Redacted reason the last refresh failed, so an operator can act on it
    /// without reading server logs. Never contains the license key.
    pub last_error: Option<String>,
}

/// `Debug` reports whether a key is stored, never the ciphertext and never the
/// plaintext, so no accidental `{:?}` can put key material in a log line.
impl std::fmt::Debug for GeoSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GeoSettings")
            .field("refresh_interval_hours", &self.refresh_interval_hours)
            .field("stale_lookup_days", &self.stale_lookup_days)
            .field("license_key_configured", &self.license_key_configured())
            .field("last_refreshed_at", &self.last_refreshed_at)
            .field("source", &self.source)
            .field("build_epoch", &self.build_epoch)
            .field("last_check_at", &self.last_check_at)
            .field("last_check_status", &self.last_check_status)
            .field("last_error", &self.last_error)
            .finish()
    }
}

impl GeoSettings {
    /// Refresh cadence actually applied, with the default and the safety
    /// bounds resolved.
    pub fn effective_refresh_interval_hours(&self) -> u32 {
        self.refresh_interval_hours
            .unwrap_or(DEFAULT_GEO_REFRESH_INTERVAL_HOURS)
            .clamp(
                MIN_GEO_REFRESH_INTERVAL_HOURS,
                MAX_GEO_REFRESH_INTERVAL_HOURS,
            )
    }

    /// [`Self::effective_refresh_interval_hours`] as a sleep duration.
    pub fn effective_refresh_interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(u64::from(self.effective_refresh_interval_hours()) * 3600)
    }

    /// Staleness window actually applied to stored IP lookups.
    pub fn effective_stale_lookup_days(&self) -> u32 {
        self.stale_lookup_days
            .unwrap_or(DEFAULT_GEO_STALE_LOOKUP_DAYS)
            .clamp(MIN_GEO_STALE_LOOKUP_DAYS, MAX_GEO_STALE_LOOKUP_DAYS)
    }

    /// Whether a MaxMind license key is stored. This is the only thing about
    /// the key that any response or log line may report.
    pub fn license_key_configured(&self) -> bool {
        self.maxmind_license_key_encrypted
            .as_deref()
            .is_some_and(|ciphertext| !ciphertext.is_empty())
    }

    /// When MaxMind built the installed data, from [`Self::build_epoch`].
    pub fn build_time(&self) -> Option<DateTime<Utc>> {
        self.build_epoch
            .and_then(|epoch| i64::try_from(epoch).ok())
            .and_then(|epoch| DateTime::from_timestamp(epoch, 0))
    }

    /// Age of the *data*, derived from [`Self::build_epoch`] when known.
    ///
    /// Preferred over `last_refreshed_at` for staleness because a download
    /// that just completed can still deliver a months-old build -- which is
    /// precisely how an instance ends up geolocating an IP to the wrong city.
    pub fn age_days(&self, now: DateTime<Utc>) -> Option<i64> {
        let reference = self.build_time().or(self.last_refreshed_at)?;
        Some((now - reference).num_days().max(0))
    }

    pub fn last_check_failed(&self) -> bool {
        self.last_check_status.as_deref() == Some(GEO_CHECK_STATUS_ERROR)
    }

    /// Restore the fields only the refresh job may write.
    ///
    /// A bulk settings save (including one built from an older GET response,
    /// which never carries these) must not be able to forge or wipe the
    /// freshness metadata `temps doctor` and `/api/geo/status` report on.
    pub fn preserve_recorded_state(&mut self, current: &GeoSettings) {
        self.last_refreshed_at = current.last_refreshed_at;
        self.source = current.source.clone();
        self.build_epoch = current.build_epoch;
        self.last_check_at = current.last_check_at;
        self.last_check_status = current.last_check_status.clone();
        self.last_error = current.last_error.clone();
    }

    /// What this (not yet applied) write intends to do to the stored license
    /// key, read from the same two write-only fields
    /// [`Self::apply_license_key_update`] consumes and with the same
    /// precedence, so the two can never disagree.
    ///
    /// Must be called *before* `apply_license_key_update`, which takes those
    /// fields; afterwards it always reports [`GeoLicenseKeyIntent::Unchanged`].
    pub fn license_key_intent(&self) -> GeoLicenseKeyIntent {
        let submitted = self
            .maxmind_license_key
            .as_deref()
            .is_some_and(|key| !key.trim().is_empty());
        if submitted {
            // A real key wins over a stale clear flag from a form that
            // submitted both, matching `apply_license_key_update`.
            GeoLicenseKeyIntent::Set
        } else if self.clear_maxmind_license_key {
            GeoLicenseKeyIntent::Cleared
        } else {
            GeoLicenseKeyIntent::Unchanged
        }
    }

    /// Resolve the incoming license-key fields against what is already stored,
    /// encrypting a newly submitted key.
    ///
    /// Mirrors the email-provider credential UX: a non-empty plaintext key
    /// replaces the stored one, a blank or absent value preserves it, and
    /// `clear_maxmind_license_key` removes it. The plaintext and the clear
    /// flag are always consumed, so the struct this leaves behind holds
    /// ciphertext only.
    pub fn apply_license_key_update(
        &mut self,
        current: &GeoSettings,
        encryption: &EncryptionService,
    ) -> Result<(), GeoSettingsError> {
        let submitted = self
            .maxmind_license_key
            .take()
            .map(|key| key.trim().to_string())
            .filter(|key| !key.is_empty());
        let clear = std::mem::take(&mut self.clear_maxmind_license_key);

        self.maxmind_license_key_encrypted = match (submitted, clear) {
            // An explicit new key always wins over a stale clear flag from a
            // form that submitted both.
            (Some(key), _) => Some(encryption.encrypt_string(&key).map_err(|e| {
                GeoSettingsError::EncryptLicenseKey {
                    reason: e.to_string(),
                }
            })?),
            (None, true) => None,
            (None, false) => current.maxmind_license_key_encrypted.clone(),
        };

        Ok(())
    }

    /// Decrypt the stored license key for the one caller that needs it: the
    /// download path building MaxMind's authenticated URL.
    ///
    /// The returned plaintext must never be logged, serialized, or placed in
    /// an error message -- see `temps_geo::refresh::redact_license_key`.
    pub fn decrypt_license_key(
        &self,
        encryption: &EncryptionService,
    ) -> Result<Option<String>, GeoSettingsError> {
        let Some(ciphertext) = self
            .maxmind_license_key_encrypted
            .as_deref()
            .filter(|ciphertext| !ciphertext.is_empty())
        else {
            return Ok(None);
        };

        let plaintext = encryption.decrypt_string(ciphertext).map_err(|e| {
            GeoSettingsError::DecryptLicenseKey {
                reason: e.to_string(),
            }
        })?;
        let plaintext = plaintext.trim().to_string();
        Ok(if plaintext.is_empty() {
            None
        } else {
            Some(plaintext)
        })
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
            public_ingress_url: None,
            preview_domain: DEFAULT_LOCAL_DOMAIN.to_string(),
            environment_hostname_template: None,
            production_hostname_template: None,
            edge_target: None,
            cloudflare_new_projects: false,
            bunny_new_projects: false,
            cloud: CloudSettings::default(),
            console_force_https: None,
            screenshots: ScreenshotSettings::default(),
            letsencrypt: LetsEncryptSettings::default(),
            dns_provider: DnsProviderSettings::default(),
            security_headers: SecurityHeadersSettings::default(),
            rate_limiting: RateLimitSettings::default(),
            trust_loopback_forwarded_ip: None,
            expose_platform_identity: None,
            loopback_ja4_header: None,
            docker_registry: DockerRegistrySettings::default(),
            registry_mirror_prefix: None,
            image_retention: ImageRetentionSettings::default(),
            disk_space_alert: DiskSpaceAlertSettings::default(),
            container_logs: ContainerLogSettings::default(),
            multi_node: MultiNodeSettings::default(),
            agent_sandbox: AgentSandboxSettings::default(),
            preview_gateway: PreviewGatewaySettings::default(),
            kv: KvSettings::default(),
            blob: BlobSettings::default(),
            // New installations opt in: stable environment URLs not covered by
            // a wildcard certificate are ordered over HTTP-01 without any setup
            // step. It still does nothing until a public zone can be derived and
            // `external_url` is not loopback, so local installs are unaffected.
            on_demand_tls: OnDemandTlsSettings {
                enabled: true,
                ..OnDemandTlsSettings::default()
            },
            ai_config: AiConfigSettings::default(),
            insecure_tls: false,
            trusted_private_networks: Vec::new(),
            ai_chat_limits: AiChatLimitsSettings::default(),
            ai_workspace_file_limits: AiWorkspaceFileLimitsSettings::default(),
            request_timeouts: RequestTimeoutSettings::default(),
            connection_limits: ConnectionLimitSettings::default(),
            tenant_resource_ceilings: TenantResourceCeilings::default(),
            build_limits: BuildLimitsSettings::default(),
            control_plane_resources: ControlPlaneResourceSettings::default(),
            cluster_dns: ClusterDnsSettings::default(),
            monitoring: MonitoringSettings::default(),
            observability_compression: ObservabilityCompressionSettings::default(),
            observability_retention: ObservabilityRetentionSettings::default(),
            analytics_retention: AnalyticsRetentionSettings::default(),
            clickhouse: ClickHouseSettings::default(),
            geo: GeoSettings::default(),
            otel_ingest: OtelIngestSettings::default(),
            backup_executor: BackupExecutorSettings::default(),
            mcp_server: McpServerSettings::default(),
            shield: ShieldSettings::default(),
            setup_complete: false,
            require_mfa_for_admins: false,
            plugin_installation_reporting_enabled: false,
            anonymous_telemetry_enabled: None,
            self_update: None,
            console_version: None,
        }
    }
}

impl AppSettings {
    /// Expose identity only when enabled, including settings saved before this field existed.
    pub fn expose_platform_identity(&self) -> bool {
        self.expose_platform_identity.unwrap_or(false)
    }

    /// The database-backed opt-in, disabled until explicitly set by an admin.
    pub fn trust_loopback_forwarded_ip(&self) -> bool {
        self.trust_loopback_forwarded_ip.unwrap_or(false)
    }

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
            cache_mb: 2048,
            head_buffer_mb: 8,
            // ~21M lines and ~21 GiB per container per day: far above what a
            // healthy service logs, low enough that one runaway container
            // cannot become the node's whole log volume.
            ingest_lines_per_sec: 250,
            ingest_kib_per_sec: 256,
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
        serde_json::from_value(value.clone()).unwrap_or_else(|_| {
            // Unrelated malformed sections must never turn a saved opt-out
            // into the opt-in default during an unrelated settings write.
            let anonymous_telemetry_enabled = match value.get("anonymous_telemetry_enabled") {
                None | Some(serde_json::Value::Null) => None,
                Some(serde_json::Value::Bool(enabled)) => Some(*enabled),
                Some(_) => Some(false),
            };
            // Preserve an explicit branding choice when another section is malformed.
            // An absent or invalid flag keeps the privacy-preserving default.
            let expose_platform_identity = value
                .get("expose_platform_identity")
                .and_then(serde_json::Value::as_bool);
            // Same for the ClickHouse wire-compression mode: resetting a saved
            // `none` to the LZ4 default would silently re-break a deployment
            // whose proxy corrupts LZ4 framing, over an unrelated bad section.
            let clickhouse = value
                .get("clickhouse")
                .and_then(|section| {
                    serde_json::from_value::<ClickHouseSettings>(section.clone()).ok()
                })
                .unwrap_or_default();
            // A stored section that does not parse falls back to the legacy
            // (off) value rather than the new-install default, for the same
            // reason `anonymous_telemetry_enabled` is preserved above.
            let on_demand_tls = value
                .get("on_demand_tls")
                .and_then(|section| {
                    serde_json::from_value::<OnDemandTlsSettings>(section.clone()).ok()
                })
                .unwrap_or_else(legacy_on_demand_tls_settings);
            // Fleet sharing is an opt-out too: a malformed section reads as
            // not sharing, never as the sharing default.
            let shield = match value.get("shield") {
                None | Some(serde_json::Value::Null) => ShieldSettings::default(),
                Some(section) => serde_json::from_value::<ShieldSettings>(section.clone())
                    .unwrap_or(ShieldSettings {
                        share_observations: false,
                    }),
            };
            Self {
                anonymous_telemetry_enabled,
                expose_platform_identity,
                clickhouse,
                on_demand_tls,
                shield,
                ..Self::default()
            }
        })
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
    #[allow(
        clippy::disallowed_methods,
        reason = "legacy runtime env config; follow-up: move to a DB-backed setting (P0.7 follow-up). TEMPS_INTERNAL_API_URL fallback after the internal_url setting"
    )]
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
    /// Reserved hosts are the console hostname (`external_url`), the public
    /// ingress hostname (`public_ingress_url`) and the preview domain apex —
    /// routing any of them at a project makes the console, every third-party
    /// callback or every generated preview URL unreachable, and recovering
    /// requires shell/IP access to the box (issue #478).
    pub fn is_reserved_hostname(&self, host: &str) -> bool {
        let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
        if host.is_empty() {
            return false;
        }
        if self.console_hostname().as_deref() == Some(host.as_str()) {
            return true;
        }
        if self.public_ingress_hostname().as_deref() == Some(host.as_str()) {
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
mod shield_settings_tests {
    use super::{AppSettings, ShieldSettings};

    #[test]
    fn a_malformed_unrelated_section_keeps_the_shield_opt_out() {
        // `geo` cannot parse, so the whole document takes the fallback.
        let stored = serde_json::json!({
            "geo": "not an object",
            "shield": { "share_observations": false }
        });
        assert!(!AppSettings::from_json(stored).shield.share_observations);

        let malformed = serde_json::json!({
            "geo": "not an object",
            "shield": { "share_observations": "maybe" }
        });
        assert!(!AppSettings::from_json(malformed).shield.share_observations);

        let absent = serde_json::json!({ "geo": "not an object" });
        assert_eq!(
            AppSettings::from_json(absent).shield,
            ShieldSettings::default()
        );
    }
}

#[cfg(test)]
mod cloudflare_new_project_tests {
    use super::AppSettings;

    #[test]
    fn legacy_settings_keep_future_projects_opted_out() {
        let settings =
            AppSettings::from_json(serde_json::json!({"preview_domain": "example.test"}));
        assert!(!settings.cloudflare_new_projects);
        assert!(!settings.bunny_new_projects);
    }

    #[test]
    fn cloudflare_default_round_trips_without_retroactive_state() {
        let settings = AppSettings {
            cloudflare_new_projects: true,
            ..Default::default()
        };
        let restored = AppSettings::from_json(settings.to_json());
        assert!(restored.cloudflare_new_projects);
    }

    #[test]
    fn bunny_default_round_trips_without_retroactive_state() {
        let settings = AppSettings {
            bunny_new_projects: true,
            ..Default::default()
        };
        let restored = AppSettings::from_json(settings.to_json());
        assert!(restored.bunny_new_projects);
        assert!(!restored.cloudflare_new_projects);
    }
}

/// Platform-wide Shield settings (ADR 066, I10).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct ShieldSettings {
    /// Whether what one environment's visitors did counts for every
    /// environment (D9): addresses and fingerprints learned from events are
    /// proposed, and an approved proposal applies fleet-wide. Nothing
    /// applies without an approval. Off, nothing is proposed or applied.
    pub share_observations: bool,
}

impl Default for ShieldSettings {
    fn default() -> Self {
        Self {
            share_observations: true,
        }
    }
}
