// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Step 3: register every plugin, in dependency order. Registration only
//! constructs them; [`initialize_plugins`] runs their two-phase start.

use super::*;

/// Where [`register_plugins`] puts each plugin, in order: the
/// [`PluginManager`] in production, a recorder in the tests that pin the
/// order.
pub(super) trait PluginRegistrar {
    fn register_plugin(&mut self, plugin: Box<dyn TempsPlugin>);
}

impl PluginRegistrar for PluginManager {
    fn register_plugin(&mut self, plugin: Box<dyn TempsPlugin>) {
        PluginManager::register_plugin(self, plugin);
    }
}

/// What [`register_plugins`] needs to construct the plugins.
pub(super) struct PluginInputs {
    pub(super) config: Arc<ServerConfig>,
    pub(super) db: Arc<DbConnection>,
    pub(super) queue: Arc<dyn temps_core::JobQueue>,
    pub(super) profile: crate::commands::serve::ServeProfile,
    pub(super) docker_available: bool,
    pub(super) external_plugin_registry: temps_external_plugins::catalog::RegistryConfig,
    pub(super) extra_plugins: Vec<Box<dyn TempsPlugin>>,
}

/// Register every plugin, then the embedding binary's extra plugins, and log
/// which ones this profile left out.
pub(super) async fn register_plugins<R: PluginRegistrar>(
    plugin_manager: &mut R,
    inputs: PluginInputs,
) -> anyhow::Result<()> {
    let PluginInputs {
        config,
        db,
        queue,
        profile,
        docker_available,
        external_plugin_registry,
        extra_plugins,
    } = inputs;

    // Whether this process runs workloads on its own host. Plugins that exist
    // only to manage local containers are not constructed at all when it is
    // false: an unconstructed plugin holds no services, spawns no background
    // loops, and costs nothing. Skipping is deliberately limited to plugins
    // whose services no kept plugin requires — the startup requirement check
    // in `PluginManager::initialize_plugins` turns any mistake here into one
    // readable error instead of a panic during initialization.
    let local_workloads = profile.local_workloads_enabled();
    let mut skipped_plugins: Vec<&'static str> = Vec::new();
    let shared_log_storage_config = register_platform_plugins(
        plugin_manager,
        &config,
        &db,
        queue,
        local_workloads,
        &mut skipped_plugins,
    )
    .await?;
    register_workload_plugins(
        plugin_manager,
        &config,
        &db,
        local_workloads,
        &mut skipped_plugins,
        external_plugin_registry,
        shared_log_storage_config,
    )
    .await?;

    // Extra plugins from the calling binary (EE, etc.). Registered last so
    // they can resolve every OSS service via `require_service`. See ADR 0001
    // §"Extension points exposed by OSS" — this is the
    // single seam between an OSS build and an EE-bundled binary.
    let extra_count = extra_plugins.len();
    for plugin in extra_plugins {
        debug!("Registering extra plugin: {}", plugin.name());
        plugin_manager.register_plugin(plugin);
    }
    if extra_count > 0 {
        info!(
            "Registered {} extra plugin(s) from binary entrypoint",
            extra_count
        );
    }

    // One line naming everything this profile left out, so an operator
    // wondering why an endpoint 404s does not have to read the source. The
    // capabilities endpoint answers the same question programmatically.
    if skipped_plugins.is_empty() {
        info!(profile = profile.as_str(), "All plugins registered");
    } else {
        info!(
            profile = profile.as_str(),
            skipped = %skipped_plugins.join(", "),
            docker_available,
            "Serve profile runs no local workloads: the listed plugins were not constructed and \
             their background tasks were not started. Applications run on worker nodes joined \
             with `temps join`; see GET /api/platform/features"
        );
    }

    Ok(())
}

/// Plugins 1 to 8: configuration, queue, logs, analytics, auth-adjacent and
/// project plugins, up to error tracking.
async fn register_platform_plugins<R: PluginRegistrar>(
    plugin_manager: &mut R,
    config: &Arc<ServerConfig>,
    db: &Arc<DbConnection>,
    queue: Arc<dyn temps_core::JobQueue>,
    local_workloads: bool,
    skipped_plugins: &mut Vec<&'static str>,
) -> anyhow::Result<StorageConfig> {
    // Register plugins in dependency order:
    // 1. ConfigPlugin - provides configuration services
    debug!("Registering ConfigPlugin");
    let config_plugin = Box::new(ConfigPlugin::new(config.clone()));
    plugin_manager.register_plugin(config_plugin);

    // Optional managed control plane. It owns the enrollment state and the
    // background telemetry mirror consumed later by OtelPlugin.
    debug!("Registering CloudPlugin");
    let cloud_plugin = Box::new(CloudPlugin::new(
        config.data_dir.clone(),
        env!("CARGO_PKG_VERSION"),
    ));
    plugin_manager.register_plugin(cloud_plugin);

    // 1.5. TelemetryPlugin - registers the anonymous telemetry reporter
    // (depends only on ServerConfig for the data dir). Registered early so
    // every later plugin can require the Arc<dyn TelemetryReporter>.
    debug!("Registering TelemetryPlugin");
    // TEMPS_VERSION (git-describe, set by build.rs) is used instead of
    // CARGO_PKG_VERSION so nightly/beta builds report a version telemetry
    // can actually distinguish from a tagged release -- CARGO_PKG_VERSION
    // is the static Cargo.toml version and is identical across all of them.
    let telemetry_plugin = Box::new(TelemetryPlugin::new(config.clone(), env!("TEMPS_VERSION")));
    plugin_manager.register_plugin(telemetry_plugin);

    // 2. QueuePlugin - registers the pre-created job queue into the service context
    debug!("Registering QueuePlugin");
    let queue_plugin = Box::new(QueuePlugin::new(queue));
    plugin_manager.register_plugin(queue_plugin);

    // 2.5. LogsPlugin - provides logging services (no dependencies)
    //
    // Resolved once here and reused for LogAggregatorPlugin below: one
    // TEMPS_LOG_STORAGE_BACKEND / TEMPS_LOG_S3_* operator decision drives
    // where Temps puts BOTH build/deploy job logs (this plugin) and
    // aggregated container logs (LogAggregatorPlugin) -- they're the same
    // "local disk or S3-compatible bucket" question, so they share one
    // config resolution instead of two independent env-var reads that could
    // drift out of sync.
    debug!("Registering LogsPlugin");
    let logs_dir = config.data_dir.join("logs");
    let stateless_instance_id = temps_config::stateless_instance_id(db.as_ref()).await?;
    let shared_log_storage_config =
        log_aggregator_storage_config(&config.data_dir, stateless_instance_id.as_deref()).map_err(
            |source| crate::commands::serve::startup_failure::ConsoleStartupError::LogStorage {
                source,
            },
        )?;
    let logs_plugin = Box::new(LogsPlugin::new(logs_dir, shared_log_storage_config.clone()));
    plugin_manager.register_plugin(logs_plugin);

    // 3.1. EventsPlugin - provides custom events tracking (depends on database)
    debug!("Registering EventsPlugin");
    let events_plugin = Box::new(EventsPlugin);
    plugin_manager.register_plugin(events_plugin);

    // 3.2. FunnelsPlugin - provides funnel analytics (depends on database)
    debug!("Registering FunnelsPlugin");
    let funnels_plugin = Box::new(FunnelsPlugin);
    plugin_manager.register_plugin(funnels_plugin);

    // 3.3. SessionReplayPlugin - provides session replay (depends on database)
    debug!("Registering SessionReplayPlugin");
    let session_replay_plugin = Box::new(SessionReplayPlugin);
    plugin_manager.register_plugin(session_replay_plugin);

    // 3.4. PerformancePlugin - provides performance metrics (depends on database)
    debug!("Registering PerformancePlugin");
    let performance_plugin = Box::new(PerformancePlugin);
    plugin_manager.register_plugin(performance_plugin);

    // 4. GeoPlugin - provides geolocation services (database validated in pre-validation)
    debug!("Registering GeoPlugin");
    let geo_plugin = Box::new(GeoPlugin::new());
    plugin_manager.register_plugin(geo_plugin);

    // 3.5. InfraPlugin - provides infrastructure and platform information (no dependencies)
    debug!("Registering InfraPlugin");
    let infra_plugin = Box::new(InfraPlugin::new());
    plugin_manager.register_plugin(infra_plugin);

    // 5. AuditPlugin - provides audit logging (depends on database and geo services)
    debug!("Registering AuditPlugin");
    let audit_plugin = Box::new(AuditPlugin::new());
    plugin_manager.register_plugin(audit_plugin);

    // 5.1. TeamsPlugin - registers project-scoped RBAC. Project-facing
    // plugins capture its ProjectAccessChecker while registering services,
    // so Teams must precede them (and follow AuditPlugin, which it requires).
    debug!("Registering TeamsPlugin");
    let teams_plugin = Box::new(TeamsPlugin::new());
    plugin_manager.register_plugin(teams_plugin);

    // 5.2. ProjectGroupsPlugin - optional parent grouping for projects. Its
    // member lists are filtered through the ProjectAccessChecker above.
    debug!("Registering ProjectGroupsPlugin");
    plugin_manager.register_plugin(Box::new(ProjectGroupsPlugin::new()));

    // 6. GitPlugin - provides git functionality (depends on other services)
    debug!("Registering GitPlugin");
    let git_plugin = Box::new(GitPlugin::new());
    plugin_manager.register_plugin(git_plugin);

    // 7. NotificationsPlugin - provides notification services (must come before AuthPlugin)
    debug!("Registering NotificationsPlugin");
    let notifications_plugin = Box::new(NotificationsPlugin::new());
    plugin_manager.register_plugin(notifications_plugin);

    // 4. DnsPlugin - provides DNS provider management (depends on database and encryption)
    // Must be registered before DomainsPlugin and EmailPlugin so DnsProviderService is available
    debug!("Registering DnsPlugin");
    let dns_plugin = Box::new(DnsPlugin::new());
    plugin_manager.register_plugin(dns_plugin);

    // 4.5. DomainsPlugin - provides TLS certificate management (depends on config, database, and DnsProviderService)
    debug!("Registering DomainsPlugin");
    let domains_plugin = Box::new(DomainsPlugin::new());
    plugin_manager.register_plugin(domains_plugin);

    // 7.1. EmailPlugin - provides email sending and domain management (depends on database, encryption, and optionally DnsProviderService)
    debug!("Registering EmailPlugin");
    let email_plugin = Box::new(EmailPlugin::new());
    plugin_manager.register_plugin(email_plugin);

    // Must follow EmailPlugin: tracking uses the email schema/services for
    // recipient correlation and domain-scoped bounce suppression.
    let email_tracking_plugin = Box::new(temps_email_tracking::EmailTrackingPlugin::new());
    plugin_manager.register_plugin(email_tracking_plugin);

    // 7.5. WebhooksPlugin - provides webhook delivery and management (depends on database and encryption)
    debug!("Registering WebhooksPlugin");
    let webhooks_plugin = Box::new(WebhooksPlugin::new());
    plugin_manager.register_plugin(webhooks_plugin);

    // 5. ProvidersPlugin - provides external service management (depends on database and encryption)
    debug!("Registering ProvidersPlugin");
    let providers_plugin = Box::new(ProvidersPlugin::new());
    plugin_manager.register_plugin(providers_plugin);

    // 5.1. KvPlugin - provides key-value storage (depends on database, docker).
    // Managed Redis runs as a container on this host, so there is nothing for
    // it to manage without local workloads.
    if local_workloads {
        debug!("Registering KvPlugin");
        let kv_plugin = Box::new(KvPlugin::new());
        plugin_manager.register_plugin(kv_plugin);
    } else {
        skipped_plugins.push("kv");
    }

    // 5.2. BlobPlugin - provides blob storage (depends on database, docker)
    debug!("Registering BlobPlugin");
    let blob_plugin = Box::new(BlobPlugin::new());
    plugin_manager.register_plugin(blob_plugin);

    // 5.3. FlagsPlugin - provides feature flags (depends on database only:
    // flags are control-plane rows, no container and no background task)
    debug!("Registering FlagsPlugin");
    let flags_plugin = Box::new(FlagsPlugin::new());
    plugin_manager.register_plugin(flags_plugin);

    // 5.4. IngressPlugin - ingress connectors such as Cloudflare Tunnel
    // (ADR 058; depends on the DNS plugin and the shared route table)
    debug!("Registering IngressPlugin");
    plugin_manager.register_plugin(Box::new(temps_ingress::IngressPlugin::new()));

    // 5.5. EnvironmentsPlugin - provides environment management (depends on config)
    debug!("Registering EnvironmentsPlugin");
    let environments_plugin = Box::new(EnvironmentsPlugin::new());
    plugin_manager.register_plugin(environments_plugin);

    // 6. ProjectsPlugin - provides project management (depends on providers, config, queue, templates)
    debug!("Registering ProjectsPlugin");
    let projects_plugin = Box::new(ProjectsPlugin::new());
    plugin_manager.register_plugin(projects_plugin);

    // 7. DeployerPlugin - provides container deployment (depends on Docker)
    debug!("Registering DeployerPlugin");
    let deployer_plugin = Box::new(DeployerPlugin::new());
    plugin_manager.register_plugin(deployer_plugin);

    // 7.5. ScreenshotsPlugin - provides screenshot capture services (depends on config)
    debug!("Registering ScreenshotsPlugin");
    let screenshots_plugin = Box::new(ScreenshotsPlugin::new());
    plugin_manager.register_plugin(screenshots_plugin);
    // 8. ErrorTrackingPlugin - provides error tracking and monitoring (includes Sentry ingestion)
    debug!("Registering ErrorTrackingPlugin");
    let error_tracking_plugin = Box::new(ErrorTrackingPlugin::new());
    plugin_manager.register_plugin(error_tracking_plugin);

    Ok(shared_log_storage_config)
}

/// Plugins 8.5 onwards: scanning, agents, AI, deployments, logs, monitoring,
/// auth, backups, proxy, external plugins and network policy.
async fn register_workload_plugins<R: PluginRegistrar>(
    plugin_manager: &mut R,
    config: &Arc<ServerConfig>,
    db: &Arc<DbConnection>,
    local_workloads: bool,
    skipped_plugins: &mut Vec<&'static str>,
    external_plugin_registry: temps_external_plugins::catalog::RegistryConfig,
    shared_log_storage_config: StorageConfig,
) -> anyhow::Result<()> {
    // 8.5. VulnerabilityScannerPlugin - provides vulnerability scanning (depends on database and audit)
    //
    // Registered in EVERY profile. Scanning a fresh image requires Trivy
    // against THIS host's local image store, and is gated internally: the
    // scanner holds a `DockerHandle` and returns a typed
    // `ScannerError::DockerUnavailable` from every scan call on a
    // control-plane process (no local daemon), rather than failing plugin
    // registration. Dropping the plugin entirely would also 404 the
    // `/vulnerability-scans` API and lose access to scan history recorded
    // before this instance was reconfigured to `control-plane`, which is a
    // control-plane responsibility, not a local-workload one.
    //
    // Scanning images that live only on a worker node is not implemented in
    // any profile today -- there is no remote scan orchestration over the
    // agent channel. `PlatformFeatures::vulnerability_scanning` is `false`
    // under `control-plane` and that is the honest, complete picture; no
    // code path here silently claims otherwise.
    debug!("Registering VulnerabilityScannerPlugin");
    let vulnerability_scanner_plugin = Box::new(VulnerabilityScannerPlugin::new());
    plugin_manager.register_plugin(vulnerability_scanner_plugin);

    // 8.6. AgentsPlugin - MUST be registered before DeploymentsPlugin so DeploymentsPlugin can
    // resolve AgentSyncService via the plugin context. If registered after, DeploymentsPlugin
    // falls back to NoOpAgentSyncService and agent sync is silently skipped on every deployment.
    //
    // Registered in EVERY profile (ADR 063). It owns the sandbox provider and
    // the worker-node resolver the sandbox API needs. Under `control-plane`
    // it never contacts Docker: the host half of the provider refuses every
    // sandbox on this host with a typed error (never an unsandboxed
    // fallback), while sandboxes on worker nodes route over their mTLS agent
    // API exactly as on a full install.
    debug!("Registering AgentsPlugin");
    let agents_plugin = Box::new(AgentsPlugin::new());
    plugin_manager.register_plugin(agents_plugin);

    // 8.7. AI Gateway Plugin - registers the provider-neutral AiService.
    // Application harness turns must see the sandbox provider registered by
    // AgentsPlugin. Registering this earlier snapshots `None` and makes every
    // sandboxed application thread fail closed for the server lifetime.
    debug!("Registering AiGatewayPlugin");
    let ai_gateway_plugin = Box::new(temps_ai_gateway::AiGatewayPlugin::new());
    plugin_manager.register_plugin(ai_gateway_plugin);

    // Analytics depends on the provider-neutral AiService, so it follows the
    // gateway registration rather than relying on an earlier incidental order.
    debug!("Registering AnalyticsPlugin");
    let analytics_plugin = Box::new(AnalyticsPlugin::new());
    plugin_manager.register_plugin(analytics_plugin);

    // 9. DeploymentsPlugin - provides deployment orchestration (depends on deployer, screenshots, and vulnerability scanner)
    debug!("Registering DeploymentsPlugin");
    let deployments_plugin = Box::new(DeploymentsPlugin::new());
    plugin_manager.register_plugin(deployments_plugin);

    // 9.0. McpServerPlugin (ADR-039) - MCP server for AI tool integration.
    // Depends on ProjectsPlugin and DeploymentsPlugin (services registered above).
    // Routes are at root level (not under /api); assembled below after plugin init.
    debug!("Registering McpServerPlugin");
    let mcp_plugin = Box::new(McpServerPlugin::new());
    plugin_manager.register_plugin(mcp_plugin);

    // 8.8. SandboxPlugin - Vercel-compatible `/v1/sandbox/*` API.
    // Consumes the SandboxProvider AgentsPlugin registers, so it goes
    // wherever AgentsPlugin goes: every profile (ADR 063). Under
    // `control-plane` placement never picks the control plane, and a create
    // with no usable worker answers the worker-node-required problem.
    debug!("Registering SandboxPlugin");
    let sandbox_plugin = Box::new(SandboxPlugin::new());
    plugin_manager.register_plugin(sandbox_plugin);

    // 9.1. LogAggregatorPlugin - structured log collection, storage, search, and streaming
    // Depends on database, Docker (from DeployerPlugin), and AuditLogger (from AuditPlugin).
    //
    // Registered in EVERY profile. Its local collector tails containers on
    // this host's daemon and is gated internally on the local-workload
    // policy, but `RemoteLogCollectorService` — which collects logs from
    // worker nodes over the agent — and the `/logs/search` and `/logs/tail`
    // routes are exactly what a control plane needs most. Dropping the plugin
    // would 404 those routes and leave worker logs uncollected, which is the
    // opposite of what this profile is for.
    debug!("Registering LogAggregatorPlugin");
    // Reuses `shared_log_storage_config`, resolved once above alongside
    // LogsPlugin -- see the comment there for why these two plugins share a
    // single config resolution instead of two independent env-var reads.
    // The ADR-047 line index uses the instance's ClickHouse connection from
    // `ServerConfig`, exactly like the other ClickHouse-backed stores above;
    // the plugin never reads the environment itself.
    let log_line_index_config = if config.is_clickhouse_enabled() {
        Some(temps_clickhouse::ClickHouseConfig::new(
            config.clickhouse_url.clone().unwrap_or_default(),
            config.clickhouse_database.clone().unwrap_or_default(),
            config.clickhouse_user.clone().unwrap_or_default(),
            config.clickhouse_password.clone().unwrap_or_default(),
        ))
    } else {
        None
    };
    let log_aggregator_plugin = Box::new(
        LogAggregatorPlugin::new(shared_log_storage_config).with_line_index(log_line_index_config),
    );
    plugin_manager.register_plugin(log_aggregator_plugin);

    // 9.5. ImportPlugin - provides workload import functionality (depends on
    // GitPlugin, ProjectsPlugin, DeploymentsPlugin).
    //
    // Every importer inspects containers on the local Docker daemon to adopt
    // an existing Compose / Coolify / Dokploy / Portainer / Kamal / CapRover
    // stack, so there is nothing for it to read here.
    if local_workloads {
        debug!("Registering ImportPlugin");
        let import_plugin = Box::new(ImportPlugin::new());
        plugin_manager.register_plugin(import_plugin);
    } else {
        skipped_plugins.push("import");
    }

    // 9.6. StatusPagePlugin - provides status page and monitoring (depends on database and projects)
    debug!("Registering StatusPagePlugin");
    let status_page_plugin = Box::new(StatusPagePlugin::new());
    plugin_manager.register_plugin(status_page_plugin);

    // 9.7. OtelPlugin - provides OpenTelemetry metrics, traces, and logs collection (depends on database)
    debug!("Registering OtelPlugin");
    let otel_plugin = Box::new(OtelPlugin::new());
    plugin_manager.register_plugin(otel_plugin);

    // 9.8. MonitoringPlugin - registers AlarmService in the service registry and
    // wires the alarms read/ack/resolve HTTP routes (ADR-025 Phase 1).
    // Must be registered AFTER NotificationsPlugin (step 7) and QueuePlugin (step 2)
    // since AlarmService requires both. Must be registered BEFORE the background
    // loops below that consume the registered AlarmService.
    debug!("Registering MonitoringPlugin");
    let monitoring_plugin = Box::new(MonitoringPlugin::new());
    plugin_manager.register_plugin(monitoring_plugin);

    // 10. AuthPlugin - provides authentication and authorization (depends on notification service)
    debug!("Registering AuthPlugin");
    let auth_plugin = Box::new(AuthPlugin::new());
    plugin_manager.register_plugin(auth_plugin);

    // 11. BackupPlugin - provides backup services (depends on database, audit, and notification services, and providers)
    //
    // Registered in EVERY profile. Only backup *execution against a container
    // on this host* depends on a local daemon, and that is gated inside the
    // plugin by the local-workload policy. Scheduling, retention, listing,
    // restore orchestration and backups of services owned by worker nodes are
    // control-plane responsibilities and must keep running here — skipping
    // the plugin would remove the entire backup API, not just local execution.
    debug!("Registering BackupPlugin");
    let backup_plugin = Box::new(BackupPlugin::new());
    plugin_manager.register_plugin(backup_plugin);

    // 11a. RevenuePlugin - per-project revenue tracking via inbound webhooks
    // (depends on database + encryption service only — no outbound API calls)
    debug!("Registering RevenuePlugin");
    let revenue_plugin = Box::new(RevenuePlugin::new());
    plugin_manager.register_plugin(revenue_plugin);

    // 11b. ObservabilityPlugin - unified Observe page (merges runtime logs,
    // requests, spans, errors, revenue into one event stream). Read-only,
    // no outbound calls; depends on the database only.
    debug!("Registering ObservabilityPlugin");
    let observability_plugin = Box::new(ObservabilityPlugin::new());
    plugin_manager.register_plugin(observability_plugin);

    // AI Chat Plugin - persistent AI debugging conversations (ADR-023). After the
    // AI gateway so the AiService it provides is registered.
    debug!("Registering AiChatPlugin");
    let ai_chat_plugin = Box::new(temps_ai_chat::AiChatPlugin::new());
    plugin_manager.register_plugin(ai_chat_plugin);

    // 12. ApiKeyPlugin - provides API key management (depends on auth services)
    debug!("Registering ApiKeyPlugin");
    let apikey_plugin = Box::new(ApiKeyPlugin::new());
    plugin_manager.register_plugin(apikey_plugin);

    // 13. ProxyPlugin - provides proxy services (depends on auth services)
    debug!("Registering ProxyPlugin");
    let proxy_plugin = Box::new(ProxyPlugin::new());
    plugin_manager.register_plugin(proxy_plugin);

    // 14. StaticFilesPlugin - provides static file serving (depends on config)
    debug!("Registering StaticFilesPlugin");
    let static_files_plugin = Box::new(StaticFilesPlugin::new());
    plugin_manager.register_plugin(static_files_plugin);

    // 15. ExternalPluginsPlugin - discovers and manages standalone binary plugins
    debug!("Registering ExternalPluginsPlugin");
    let mut external_plugin_config = temps_external_plugins::manager::ExternalPluginConfig::new(
        config.data_dir.clone(),
        config.database_url.clone(),
    )
    // Where this instance answers HTTP. A plugin's routes are only reachable
    // through the proxy, so a plugin that has to hand out a URL to something
    // outside the request (a sandboxed agent, a webhook receiver) cannot
    // construct one without being told the address the proxy listens on.
    .with_proxy_address(&config.address)
    .with_registry(external_plugin_registry);
    external_plugin_config.persistent_installations = !temps_config::installation_mode(db.as_ref())
        .await?
        .is_stateless();
    let external_plugins_plugin = Box::new(temps_external_plugins::ExternalPluginsPlugin::new(
        external_plugin_config,
    ));
    plugin_manager.register_plugin(external_plugins_plugin);

    // NetworkPolicyPlugin - network policy rules for every workload kind,
    // per-node modes, and node 0's enforcer (ADR-052 §4, ADR-055 phase 7).
    // Every node defaults to `legacy`, where the enforcer only reads the
    // mode: no Docker call, no nftables.
    debug!("Registering NetworkPolicyPlugin");
    plugin_manager.register_plugin(Box::new(temps_network_policy::NetworkPolicyPlugin::new()));

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::serve::ServeProfile;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use std::collections::BTreeMap;

    /// Records plugin names in registration order instead of constructing a
    /// `PluginManager`.
    #[derive(Default)]
    struct Recorder(Vec<&'static str>);

    impl PluginRegistrar for Recorder {
        fn register_plugin(&mut self, plugin: Box<dyn TempsPlugin>) {
            self.0.push(plugin.name());
        }
    }

    /// A plugin standing in for one the embedding binary passes in.
    struct ExtraPlugin;

    impl TempsPlugin for ExtraPlugin {
        fn name(&self) -> &'static str {
            "extra"
        }

        fn register_services<'a>(
            &'a self,
            _context: &'a temps_core::plugin::ServiceRegistrationContext,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<(), temps_core::plugin::PluginError>>
                    + Send
                    + 'a,
            >,
        > {
            Box::pin(async { Ok(()) })
        }
    }

    /// The order every plugin is registered in under the `full` profile.
    /// Registration order is the dependency order (a plugin resolves services
    /// registered before it), so a change here must be deliberate.
    const FULL_ORDER: &[&str] = &[
        "config",
        "cloud",
        "telemetry",
        "queue",
        "logs",
        "analytics-events",
        "funnels",
        "session-replay",
        "performance",
        "geo",
        "infra",
        "audit",
        "teams",
        "project-groups",
        "git",
        "notifications",
        "dns",
        "domains",
        "email",
        "email-tracking",
        "webhooks",
        "providers",
        "kv",
        "blob",
        "flags",
        "ingress",
        "environments",
        "projects",
        "deployer",
        "screenshots",
        "error-tracking",
        "vulnerability-scanner",
        "agents",
        "ai_gateway",
        "analytics",
        "deployments",
        "mcp-server",
        "sandbox",
        "log-aggregator",
        "import",
        "status-page",
        "otel",
        "monitoring",
        "auth",
        "backup",
        "revenue",
        "observability",
        "ai_chat",
        "apikey",
        "proxy",
        "static-files",
        "external-plugins",
        "network-policy",
    ];

    /// One `{ present: false }` row: the stateless identity table is absent.
    fn local_installation_row() -> BTreeMap<String, sea_orm::Value> {
        BTreeMap::from([("present".to_string(), sea_orm::Value::Bool(Some(false)))])
    }

    /// Registration reads the installation mode twice (for the log storage
    /// config and for external plugin persistence); both answer "local".
    fn local_installation_db() -> Arc<DbConnection> {
        Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([[local_installation_row()], [local_installation_row()]])
                .into_connection(),
        )
    }

    fn inputs(
        data_dir: &std::path::Path,
        profile: ServeProfile,
        extra_plugins: Vec<Box<dyn TempsPlugin>>,
    ) -> PluginInputs {
        let mut config = ServerConfig::new(
            "127.0.0.1:3000".to_string(),
            "postgres://localhost:5432/test".to_string(),
            None,
            Some("127.0.0.1:3001".to_string()),
        )
        .expect("server config builds");
        config.data_dir = data_dir.to_path_buf();
        PluginInputs {
            config: Arc::new(config),
            db: local_installation_db(),
            queue: temps_queue::BroadcastQueueService::create_job_queue_arc(16),
            profile,
            docker_available: false,
            external_plugin_registry: temps_external_plugins::catalog::RegistryConfig::default(),
            extra_plugins,
        }
    }

    async fn registered(
        profile: ServeProfile,
        extra_plugins: Vec<Box<dyn TempsPlugin>>,
    ) -> Vec<&'static str> {
        let data_dir = tempfile::tempdir().expect("temp data dir");
        let mut recorder = Recorder::default();
        register_plugins(
            &mut recorder,
            inputs(data_dir.path(), profile, extra_plugins),
        )
        .await
        .expect("plugins register");
        recorder.0
    }

    #[tokio::test]
    async fn full_profile_registers_every_plugin_in_dependency_order() {
        assert_eq!(registered(ServeProfile::Full, Vec::new()).await, FULL_ORDER);
    }

    #[tokio::test]
    async fn control_plane_profile_skips_only_the_local_container_plugins() {
        let expected: Vec<&str> = FULL_ORDER
            .iter()
            .copied()
            .filter(|name| !matches!(*name, "kv" | "import"))
            .collect();
        assert_eq!(
            registered(ServeProfile::ControlPlane, Vec::new()).await,
            expected
        );
    }

    #[tokio::test]
    async fn extra_plugins_register_after_every_built_in_plugin() {
        let names = registered(ServeProfile::Full, vec![Box::new(ExtraPlugin)]).await;
        assert_eq!(names.last(), Some(&"extra"));
        assert_eq!(&names[..names.len() - 1], FULL_ORDER);
    }

    #[tokio::test]
    async fn an_installation_mode_read_failure_stops_registration() {
        let data_dir = tempfile::tempdir().expect("temp data dir");
        let mut inputs = inputs(data_dir.path(), ServeProfile::Full, Vec::new());
        inputs.db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_errors([sea_orm::DbErr::Custom("connection refused".to_string())])
                .into_connection(),
        );
        let mut recorder = Recorder::default();
        let error = register_plugins(&mut recorder, inputs)
            .await
            .expect_err("a database error must fail startup");
        assert!(
            error.to_string().contains("installation"),
            "the error names what was being read: {error}"
        );
        // Nothing after LogsPlugin's config resolution was registered.
        assert_eq!(recorder.0, ["config", "cloud", "telemetry", "queue"]);
    }
}
