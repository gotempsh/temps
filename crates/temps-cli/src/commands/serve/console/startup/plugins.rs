// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Step 3: register every plugin, in dependency order. Registration only
//! constructs them; [`initialize_plugins`] runs their two-phase start.
//!
//! The order is [`register_builtin_plugins`] read top to bottom. A plugin that
//! needs nothing from startup is one row in a table; one that takes inputs or
//! depends on the profile is an explicit statement between the tables.

use super::*;

/// One row of a plugin table: builds a plugin that needs no startup inputs.
type PluginRow = fn() -> Box<dyn TempsPlugin>;

/// Where [`register_plugins`] puts each plugin, in order: the
/// [`PluginManager`] in production, a recorder in the tests that pin the
/// order.
pub(super) trait PluginRegistrar {
    fn register_plugin(&mut self, plugin: Box<dyn TempsPlugin>);

    /// Build and register each row, top to bottom.
    fn register_table(&mut self, table: &[PluginRow]) {
        for build in table {
            self.register_plugin(build());
        }
    }
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
    let skipped_plugins = register_builtin_plugins(
        plugin_manager,
        &config,
        &db,
        queue,
        local_workloads,
        external_plugin_registry,
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

/// Register the built-in plugins and return the names this profile skipped.
///
/// This function is the startup order: a plugin resolves services registered
/// before it, so a row or statement must stay after what it depends on. To add
/// a plugin that needs no startup inputs, add a row at the point its
/// dependencies are met.
async fn register_builtin_plugins<R: PluginRegistrar>(
    plugin_manager: &mut R,
    config: &Arc<ServerConfig>,
    db: &Arc<DbConnection>,
    queue: Arc<dyn temps_core::JobQueue>,
    local_workloads: bool,
    external_plugin_registry: temps_external_plugins::catalog::RegistryConfig,
) -> anyhow::Result<Vec<&'static str>> {
    let mut skipped_plugins: Vec<&'static str> = Vec::new();

    // Provides configuration services.
    plugin_manager.register_plugin(Box::new(ConfigPlugin::new(config.clone())));

    // Optional managed control plane. It owns the enrollment state and the
    // background telemetry mirror consumed later by OtelPlugin.
    plugin_manager.register_plugin(Box::new(CloudPlugin::new(
        config.data_dir.clone(),
        env!("CARGO_PKG_VERSION"),
    )));

    // Registers the anonymous telemetry reporter (depends only on ServerConfig
    // for the data dir). Registered early so every later plugin can require
    // the Arc<dyn TelemetryReporter>.
    //
    // TEMPS_VERSION (git-describe, set by build.rs) is used instead of
    // CARGO_PKG_VERSION so nightly/beta builds report a version telemetry
    // can actually distinguish from a tagged release -- CARGO_PKG_VERSION
    // is the static Cargo.toml version and is identical across all of them.
    plugin_manager.register_plugin(Box::new(TelemetryPlugin::new(
        config.clone(),
        env!("TEMPS_VERSION"),
    )));

    // Registers the pre-created job queue into the service context.
    plugin_manager.register_plugin(Box::new(QueuePlugin::new(queue)));

    // Provides logging services (no dependencies).
    //
    // Resolved once here and reused for LogAggregatorPlugin below: one
    // TEMPS_LOG_STORAGE_BACKEND / TEMPS_LOG_S3_* operator decision drives
    // where Temps puts BOTH build/deploy job logs (this plugin) and
    // aggregated container logs (LogAggregatorPlugin) -- they're the same
    // "local disk or S3-compatible bucket" question, so they share one
    // config resolution instead of two independent env-var reads that could
    // drift out of sync.
    let stateless_instance_id = temps_config::stateless_instance_id(db.as_ref()).await?;
    let shared_log_storage_config =
        log_aggregator_storage_config(&config.data_dir, stateless_instance_id.as_deref()).map_err(
            |source| crate::commands::serve::startup_failure::ConsoleStartupError::LogStorage {
                source,
            },
        )?;
    plugin_manager.register_plugin(Box::new(LogsPlugin::new(
        config.data_dir.join("logs"),
        shared_log_storage_config.clone(),
    )));

    plugin_manager.register_table(&[
        // Custom events tracking (depends on database).
        || Box::new(EventsPlugin),
        // Funnel analytics (depends on database).
        || Box::new(FunnelsPlugin),
        // Session replay (depends on database).
        || Box::new(SessionReplayPlugin),
        // Performance metrics (depends on database).
        || Box::new(PerformancePlugin),
        // Geolocation services (database validated in pre-validation).
        || Box::new(GeoPlugin::new()),
        // Infrastructure and platform information (no dependencies).
        || Box::new(InfraPlugin::new()),
        // Audit logging (depends on database and geo services).
        || Box::new(AuditPlugin::new()),
        // Project-scoped RBAC. Project-facing plugins capture its
        // ProjectAccessChecker while registering services, so Teams must
        // precede them (and follow AuditPlugin, which it requires).
        || Box::new(TeamsPlugin::new()),
        // Optional parent grouping for projects. Its member lists are filtered
        // through the ProjectAccessChecker above.
        || Box::new(ProjectGroupsPlugin::new()),
        // Git functionality (depends on other services).
        || Box::new(GitPlugin::new()),
        // Notification services (must come before AuthPlugin).
        || Box::new(NotificationsPlugin::new()),
        // DNS provider management (depends on database and encryption). Must
        // be before DomainsPlugin and EmailPlugin so DnsProviderService is
        // available.
        || Box::new(DnsPlugin::new()),
        // TLS certificate management (depends on config, database, and
        // DnsProviderService).
        || Box::new(DomainsPlugin::new()),
        // Email sending and domain management (depends on database, encryption,
        // and optionally DnsProviderService).
        || Box::new(EmailPlugin::new()),
        // Must follow EmailPlugin: tracking uses the email schema/services for
        // recipient correlation and domain-scoped bounce suppression.
        || Box::new(temps_email_tracking::EmailTrackingPlugin::new()),
        // Webhook delivery and management (depends on database and encryption).
        || Box::new(WebhooksPlugin::new()),
        // External service management (depends on database and encryption).
        || Box::new(ProvidersPlugin::new()),
    ]);

    // Key-value storage (depends on database, docker). Managed Redis runs as a
    // container on this host, so there is nothing for it to manage without
    // local workloads.
    if local_workloads {
        plugin_manager.register_plugin(Box::new(KvPlugin::new()));
    } else {
        skipped_plugins.push("kv");
    }

    plugin_manager.register_table(&[
        // Blob storage (depends on database, docker).
        || Box::new(BlobPlugin::new()),
        // Feature flags (depends on database only: flags are control-plane
        // rows, no container and no background task).
        || Box::new(FlagsPlugin::new()),
        // Ingress connectors such as Cloudflare Tunnel (ADR 058; depends on the
        // DNS plugin and the shared route table).
        || Box::new(temps_ingress::IngressPlugin::new()),
    ]);

    // Environment management (depends on config). Not a table row: it takes the
    // profile's local-workload setting.
    plugin_manager.register_plugin(Box::new(
        EnvironmentsPlugin::new().with_local_workloads(local_workloads),
    ));

    plugin_manager.register_table(&[
        // Project management (depends on providers, config, queue, templates).
        || Box::new(ProjectsPlugin::new()),
        // Container deployment (depends on Docker).
        || Box::new(DeployerPlugin::new()),
        // Screenshot capture services (depends on config).
        || Box::new(ScreenshotsPlugin::new()),
        // Error tracking and monitoring (includes Sentry ingestion).
        || Box::new(ErrorTrackingPlugin::new()),
        // Vulnerability scanning (depends on database and audit).
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
        || Box::new(VulnerabilityScannerPlugin::new()),
        // MUST be registered before DeploymentsPlugin so DeploymentsPlugin can
        // resolve AgentSyncService via the plugin context. If registered after,
        // DeploymentsPlugin falls back to NoOpAgentSyncService and agent sync is
        // silently skipped on every deployment.
        //
        // Registered in EVERY profile (ADR 063). It owns the sandbox provider
        // and the worker-node resolver the sandbox API needs. Under
        // `control-plane` it never contacts Docker: the host half of the
        // provider refuses every sandbox on this host with a typed error (never
        // an unsandboxed fallback), while sandboxes on worker nodes route over
        // their mTLS agent API exactly as on a full install.
        || Box::new(AgentsPlugin::new()),
        // Registers the provider-neutral AiService. Application harness turns
        // must see the sandbox provider registered by AgentsPlugin. Registering
        // this earlier snapshots `None` and makes every sandboxed application
        // thread fail closed for the server lifetime.
        || Box::new(temps_ai_gateway::AiGatewayPlugin::new()),
        // Analytics depends on the provider-neutral AiService, so it follows
        // the gateway registration rather than relying on an earlier incidental
        // order.
        || Box::new(AnalyticsPlugin::new()),
        // Deployment orchestration (depends on deployer, screenshots, and
        // vulnerability scanner).
        || Box::new(DeploymentsPlugin::new()),
        // MCP server for AI tool integration (ADR-039). Depends on
        // ProjectsPlugin and DeploymentsPlugin (services registered above).
        // Routes are at root level (not under /api); assembled after plugin
        // init.
        || Box::new(McpServerPlugin::new()),
        // Vercel-compatible `/v1/sandbox/*` API. Consumes the SandboxProvider
        // AgentsPlugin registers, so it goes wherever AgentsPlugin goes: every
        // profile (ADR 063). Under `control-plane` placement never picks the
        // control plane, and a create with no usable worker answers the
        // worker-node-required problem.
        || Box::new(SandboxPlugin::new()),
    ]);

    // Structured log collection, storage, search, and streaming. Depends on
    // database, Docker (from DeployerPlugin), and AuditLogger (from
    // AuditPlugin).
    //
    // Registered in EVERY profile. Its local collector tails containers on
    // this host's daemon and is gated internally on the local-workload
    // policy, but `RemoteLogCollectorService` — which collects logs from
    // worker nodes over the agent — and the `/logs/search` and `/logs/tail`
    // routes are exactly what a control plane needs most. Dropping the plugin
    // would 404 those routes and leave worker logs uncollected, which is the
    // opposite of what this profile is for.
    plugin_manager.register_plugin(Box::new(log_aggregator_plugin(
        config,
        shared_log_storage_config,
    )));

    // Workload import (depends on GitPlugin, ProjectsPlugin, DeploymentsPlugin).
    //
    // Every importer inspects containers on the local Docker daemon to adopt
    // an existing Compose / Coolify / Dokploy / Portainer / Kamal / CapRover
    // stack, so there is nothing for it to read here.
    if local_workloads {
        plugin_manager.register_plugin(Box::new(ImportPlugin::new()));
    } else {
        skipped_plugins.push("import");
    }

    plugin_manager.register_table(&[
        // Status page and monitoring (depends on database and projects).
        || Box::new(StatusPagePlugin::new()),
        // OpenTelemetry metrics, traces, and logs collection (depends on
        // database).
        || Box::new(OtelPlugin::new()),
        // Registers AlarmService in the service registry and wires the alarms
        // read/ack/resolve HTTP routes (ADR-025 Phase 1). Must be registered
        // AFTER NotificationsPlugin and QueuePlugin since AlarmService requires
        // both. Must be registered BEFORE the background loops that consume the
        // registered AlarmService.
        || Box::new(MonitoringPlugin::new()),
        // Authentication and authorization (depends on notification service).
        || Box::new(AuthPlugin::new()),
        // Backup services (depends on database, audit, and notification
        // services, and providers).
        //
        // Registered in EVERY profile. Only backup *execution against a
        // container on this host* depends on a local daemon, and that is gated
        // inside the plugin by the local-workload policy. Scheduling, retention,
        // listing, restore orchestration and backups of services owned by
        // worker nodes are control-plane responsibilities and must keep running
        // here — skipping the plugin would remove the entire backup API, not
        // just local execution.
        || Box::new(BackupPlugin::new()),
        // Per-project revenue tracking via inbound webhooks (depends on
        // database + encryption service only — no outbound API calls).
        || Box::new(RevenuePlugin::new()),
        // Unified Observe page (merges runtime logs, requests, spans, errors,
        // revenue into one event stream). Read-only, no outbound calls; depends
        // on the database only.
        || Box::new(ObservabilityPlugin::new()),
        // Persistent AI debugging conversations (ADR-023). After the AI gateway
        // so the AiService it provides is registered.
        || Box::new(temps_ai_chat::AiChatPlugin::new()),
        // API key management (depends on auth services).
        || Box::new(ApiKeyPlugin::new()),
        // Proxy services (depends on auth services).
        || Box::new(ProxyPlugin::new()),
        // Static file serving (depends on config).
        || Box::new(StaticFilesPlugin::new()),
    ]);

    // Discovers and manages standalone binary plugins.
    let persistent_installations = !temps_config::installation_mode(db.as_ref())
        .await?
        .is_stateless();
    plugin_manager.register_plugin(Box::new(external_plugins_plugin(
        config,
        external_plugin_registry,
        persistent_installations,
    )));

    plugin_manager.register_table(&[
        // Network policy rules for every workload kind, per-node modes, and
        // node 0's enforcer (ADR-052 §4, ADR-055 phase 7). Every node defaults
        // to `legacy`, where the enforcer only reads the mode: no Docker call,
        // no nftables.
        || Box::new(temps_network_policy::NetworkPolicyPlugin::new()),
    ]);

    Ok(skipped_plugins)
}

/// LogAggregatorPlugin over `log_storage_config`, the resolution it shares with
/// LogsPlugin (see [`register_builtin_plugins`] for why).
///
/// The ADR-047 line index uses the instance's ClickHouse connection from
/// `ServerConfig`, exactly like the other ClickHouse-backed stores; the plugin
/// never reads the environment itself.
fn log_aggregator_plugin(
    config: &ServerConfig,
    log_storage_config: StorageConfig,
) -> LogAggregatorPlugin {
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
    LogAggregatorPlugin::new(log_storage_config).with_line_index(log_line_index_config)
}

fn external_plugins_plugin(
    config: &ServerConfig,
    registry: temps_external_plugins::catalog::RegistryConfig,
    persistent_installations: bool,
) -> temps_external_plugins::ExternalPluginsPlugin {
    let mut external_plugin_config = temps_external_plugins::manager::ExternalPluginConfig::new(
        config.data_dir.clone(),
        config.database_url.clone(),
    )
    // Where this instance answers HTTP. A plugin's routes are only reachable
    // through the proxy, so a plugin that has to hand out a URL to something
    // outside the request (a sandboxed agent, a webhook receiver) cannot
    // construct one without being told the address the proxy listens on.
    .with_proxy_address(&config.address)
    .with_registry(registry);
    external_plugin_config.persistent_installations = persistent_installations;
    // `--plugin-launcher`, bridged into the environment by `serve`.
    if let Some(launcher) =
        std::env::var_os("TEMPS_PLUGIN_LAUNCHER").filter(|value| !value.is_empty())
    {
        tracing::info!(
            launcher = %std::path::Path::new(&launcher).display(),
            "External plugins run as a separate user through the plugin launcher"
        );
        external_plugin_config = external_plugin_config.with_launcher(
            temps_external_plugins::launcher::PluginLauncher::new(launcher),
        );
    }
    temps_external_plugins::ExternalPluginsPlugin::new(external_plugin_config)
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

    async fn skipped_by(profile: ServeProfile) -> Vec<&'static str> {
        let data_dir = tempfile::tempdir().expect("temp data dir");
        let inputs = inputs(data_dir.path(), profile, Vec::new());
        register_builtin_plugins(
            &mut Recorder::default(),
            &inputs.config,
            &inputs.db,
            inputs.queue.clone(),
            inputs.profile.local_workloads_enabled(),
            inputs.external_plugin_registry,
        )
        .await
        .expect("plugins register")
    }

    #[tokio::test]
    async fn only_the_profile_gated_plugins_are_reported_as_skipped() {
        assert!(skipped_by(ServeProfile::Full).await.is_empty());
        assert_eq!(
            skipped_by(ServeProfile::ControlPlane).await,
            ["kv", "import"]
        );
    }

    /// Keeps the environments plugin, which the name-only [`Recorder`] drops, so
    /// a test can start it.
    #[derive(Default)]
    struct EnvironmentsPluginKeeper(Option<Box<dyn TempsPlugin>>);

    impl PluginRegistrar for EnvironmentsPluginKeeper {
        fn register_plugin(&mut self, plugin: Box<dyn TempsPlugin>) {
            if plugin.name() == "environments" {
                self.0 = Some(plugin);
            }
        }
    }

    /// An environment asking to spread two replicas, on an instance with no
    /// worker nodes: the control plane is the only node that could take one.
    fn two_spread_replicas_environment() -> temps_entities::environments::Model {
        temps_entities::environments::Model {
            id: 1,
            name: "staging".to_string(),
            slug: "staging".to_string(),
            subdomain: "staging".to_string(),
            branch: None,
            project_id: 10,
            host: String::new(),
            upstreams: temps_entities::upstream_config::UpstreamList::new(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            last_deployment: None,
            current_deployment_id: None,
            deleted_at: None,
            deployment_config: Some(temps_entities::deployment_config::DeploymentConfig {
                replicas: 2,
                anti_affinity: true,
                ..Default::default()
            }),
            is_preview: false,
            protected: false,
            sleeping: false,
            attack_mode: None,
            shield_mode: None,
            shield_exempt_paths: Default::default(),
            force_https: None,
            last_activity_at: None,
            ingress_mode: Default::default(),
            workload_mode: Default::default(),
        }
    }

    /// Starts the environments plugin [`register_plugins`] builds for `profile`
    /// and returns the placement warning its service gives for
    /// [`two_spread_replicas_environment`].
    async fn placement_warning_under(profile: ServeProfile) -> String {
        let data_dir = tempfile::tempdir().expect("temp data dir");
        let inputs = inputs(data_dir.path(), profile, Vec::new());
        let config = inputs.config.clone();
        let queue = inputs.queue.clone();
        let mut keeper = EnvironmentsPluginKeeper::default();
        register_plugins(&mut keeper, inputs)
            .await
            .expect("plugins register");
        let plugin = keeper.0.expect("the environments plugin is registered");

        // Answers the warning's two reads: no project row, no worker nodes.
        let db: Arc<DbConnection> = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([Vec::<BTreeMap<String, sea_orm::Value>>::new(), Vec::new()])
                .into_connection(),
        );
        let context = temps_core::plugin::ServiceRegistrationContext::new();
        context.register_service(db.clone());
        context.register_service(Arc::new(temps_config::ConfigService::new(config, db)));
        context.register_service(queue);
        context.register_service(Arc::new(temps_core::EncryptionService::new_from_password(
            "test",
        )));
        plugin
            .register_services(&context)
            .await
            .expect("the environments plugin registers its services");

        context
            .require_service::<temps_environments::EnvironmentService>()
            .placement_warning(&two_spread_replicas_environment())
            .await
            .expect("two replicas never fit on at most one node")
    }

    /// The setting reaches the plugin through an explicit statement a table row
    /// can't express, and the plugin defaults to `true`, so losing it only shows
    /// under `control-plane`: the service would count a control plane that runs
    /// no workloads as a place for a replica. Asserted on what the service
    /// reports, so the whole path from the profile is covered.
    #[tokio::test]
    async fn environments_plugin_gets_the_profiles_local_workload_setting() {
        let full = placement_warning_under(ServeProfile::Full).await;
        assert!(
            full.contains("1 eligible node(s)"),
            "the full profile's control plane takes a replica: {full}"
        );

        let control_plane = placement_warning_under(ServeProfile::ControlPlane).await;
        assert!(
            control_plane.contains("No eligible node"),
            "the control-plane profile runs no local workloads: {control_plane}"
        );
    }

    /// Goes through the real [`PluginManager`], not the recorder. Phase 1 has
    /// not run, so nothing is in the service registry and every declared
    /// requirement of every registered plugin is unmet.
    #[tokio::test]
    async fn a_missing_required_service_fails_with_the_requirement_error() {
        let data_dir = tempfile::tempdir().expect("temp data dir");
        let mut plugin_manager = PluginManager::new();
        register_plugins(
            &mut plugin_manager,
            inputs(data_dir.path(), ServeProfile::Full, Vec::new()),
        )
        .await
        .expect("plugins register");

        let error = plugin_manager
            .verify_required_services()
            .expect_err("no service is registered yet");

        assert!(
            matches!(
                error,
                temps_core::plugin::PluginError::MissingRequiredServices { .. }
            ),
            "the requirement check reports its own error: {error}"
        );
        let message = error.to_string();
        assert!(
            message.contains("require services that no registered plugin provides"),
            "the error says what is wrong: {message}"
        );
        for plugin in ["providers", "deployer", "error-tracking", "deployments"] {
            assert!(
                message.contains(&format!("  - {plugin}: ")),
                "the error names {plugin}: {message}"
            );
        }
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
