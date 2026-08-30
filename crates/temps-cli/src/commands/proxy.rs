// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use clap::Args;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use temps_config::ServerConfig;
use temps_core::CookieCrypto;
use temps_database::DbConnection;
use temps_proxy::on_demand::{ContainerLifecycle, OnDemandManager};
use temps_proxy::ProxyShutdownSignal;
use tracing::{debug, error, info, warn};

/// Outcome of comparing the running proxy binary version against the console
/// version recorded in settings (ADR-017 Phase 3). Pure/total — never panics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkewStatus {
    /// Proxy and console report the same version tag.
    Match,
    /// Versions differ — schema-skew risk during a rolling upgrade.
    Skew { proxy: String, console: String },
    /// Console version not recorded (absent/None/empty) — cannot compare.
    Unknown,
}

/// Compare the proxy's binary version against the console's recorded version.
///
/// Total function: `None` (or empty/whitespace-only) console → `Unknown`;
/// equal tags → `Match`; otherwise → `Skew`. Comparison is exact string
/// equality on the already-normalized tags produced by
/// [`crate::commands::upgrade::current_version_tag`] (both processes are the
/// same binary family, so the tag formats are identical). Surrounding
/// whitespace is trimmed defensively. No indexing, no unwrap, no parse —
/// arbitrary/garbage input cannot panic.
pub fn compare_versions(proxy: &str, console: Option<&str>) -> SkewStatus {
    match console {
        None => SkewStatus::Unknown,
        Some(console) => {
            let p = proxy.trim();
            let c = console.trim();
            if c.is_empty() {
                // Treat an empty recorded version as "not recorded" rather than
                // a skew, to avoid a false alarm if some path ever writes `""`.
                SkewStatus::Unknown
            } else if p == c {
                SkewStatus::Match
            } else {
                SkewStatus::Skew {
                    proxy: p.to_string(),
                    console: c.to_string(),
                }
            }
        }
    }
}

/// Shutdown signal implementation for Ctrl+C with resource cleanup and timeout
struct CtrlCShutdownSignal {
    cleanup_timeout: Duration,
    db: Arc<DbConnection>,
    data_dir: PathBuf,
}

impl CtrlCShutdownSignal {
    fn new(cleanup_timeout: Duration, db: Arc<DbConnection>, data_dir: PathBuf) -> Self {
        Self {
            cleanup_timeout,
            db,
            data_dir,
        }
    }

    /// Perform cleanup operations with timeout
    async fn cleanup_resources(&self) {
        info!("Starting resource cleanup...");

        let cleanup_future = async {
            // Database cleanup
            self.cleanup_database().await;

            // File system cleanup
            self.cleanup_files().await;

            info!("Resource cleanup completed successfully");
        };

        // Apply timeout to cleanup operations
        match tokio::time::timeout(self.cleanup_timeout, cleanup_future).await {
            Ok(()) => {
                info!("All resources cleaned up within timeout");
            }
            Err(_) => {
                warn!(
                    "Cleanup timeout exceeded ({:?}), forcing shutdown",
                    self.cleanup_timeout
                );
            }
        }
    }

    async fn cleanup_database(&self) {
        debug!("Cleaning up database connections...");

        // Close the database connection gracefully
        // if let Err(e) = &self.db.close().await {
        //     warn!("Error closing database connection: {}", e);
        // } else {
        //     debug!("Database connection closed successfully");
        // }

        debug!("Database cleanup completed");
    }

    async fn cleanup_files(&self) {
        debug!("Cleaning up temporary files...");

        // Flush log buffers
        // Note: In a real implementation, you'd have access to the subscriber handle to flush
        debug!("Log buffers flushed");

        // Clean up any temporary files in data directory
        let temp_dir = self.data_dir.join("temp");
        if temp_dir.exists() {
            if let Err(e) = std::fs::remove_dir_all(&temp_dir) {
                warn!("Failed to remove temp directory: {}", e);
            } else {
                debug!("Temporary files cleaned up");
            }
        }

        debug!("File cleanup completed");
    }
}

impl ProxyShutdownSignal for CtrlCShutdownSignal {
    fn wait_for_signal(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
        let cleanup_timeout = self.cleanup_timeout;
        let db = Arc::clone(&self.db);
        let data_dir = self.data_dir.clone();

        Box::pin(async move {
            tokio::signal::ctrl_c()
                .await
                .expect("Failed to listen for ctrl-c signal");
            info!("Received Ctrl+C, initiating graceful shutdown...");

            // Create a new instance for cleanup since we moved into the async block
            let cleanup_handler = CtrlCShutdownSignal::new(cleanup_timeout, db, data_dir);
            cleanup_handler.cleanup_resources().await;

            info!("Graceful shutdown completed");
        })
    }
}

#[derive(Args)]
pub struct ProxyCommand {
    /// Address to bind the server to
    #[arg(long, default_value = "127.0.0.1:3000", env = "TEMPS_ADDRESS")]
    pub address: String,

    /// TLS address to bind the server to
    #[arg(long, env = "TEMPS_TLS_ADDRESS")]
    pub tls_address: Option<String>,

    /// Database connection URL (set via TEMPS_DATABASE_URL env var; not accepted as a flag to prevent credentials leaking into process listings)
    #[arg(long, env = "TEMPS_DATABASE_URL", hide_env_values = true)]
    pub database_url: String,

    /// Data directory for storing configuration and runtime files
    #[arg(long, env = "TEMPS_DATA_DIR")]
    pub data_dir: Option<PathBuf>,

    /// Console/Admin address (defaults to random port on localhost)
    #[arg(long, env = "TEMPS_CONSOLE_ADDRESS")]
    pub console_address: Option<String>,

    /// Disable HTTP-to-HTTPS redirect (useful for local development without TLS)
    #[arg(long, env = "TEMPS_DISABLE_HTTPS_REDIRECT")]
    pub disable_https_redirect: bool,
}

/// Builds the [`temps_core::ProjectIpGate`] a standalone proxy enforces with.
///
/// Called once at startup with the proxy's own database handle and the
/// runtime handle its background work may use. This exists because the
/// standalone proxy has no plugin registry — the mechanism every other
/// process uses to install a gate — but it *does* have the same database,
/// so there is no structural reason it must under-enforce. A binary that
/// knows how to build a gate passes one here; `execute` passes none and
/// gets [`temps_core::OpenIpGate`].
///
/// The runtime handle is part of the contract rather than an implementation
/// detail: pingora owns the process runtime once the proxy starts, so a gate
/// that refreshes in the background has to be given somewhere to run before
/// that happens.
pub type ProjectIpGateBuilder = Box<
    dyn FnOnce(Arc<DbConnection>, &tokio::runtime::Handle) -> Arc<dyn temps_core::ProjectIpGate>,
>;

impl ProxyCommand {
    pub fn execute(self) -> anyhow::Result<()> {
        self.execute_with_ip_gate(None)
    }

    /// `execute`, but with a caller-supplied project IP gate.
    pub fn execute_with_ip_gate(
        self,
        ip_gate_builder: Option<ProjectIpGateBuilder>,
    ) -> anyhow::Result<()> {
        let runtime_context = Arc::new(temps_core::initialize_process_runtime_context()?.clone());
        if runtime_context.source() == temps_core::ExecutionEnvironmentSource::Legacy {
            warn!(
                legacy_variable = temps_core::LEGACY_DEPLOYMENT_MODE_VARIABLE,
                canonical_variable = temps_core::EXECUTION_ENVIRONMENT_VARIABLE,
                execution_environment = %runtime_context.execution_environment(),
                "Using deprecated execution-environment configuration; migrate to TEMPS_EXECUTION_ENV"
            );
        }

        let serve_config = Arc::new(temps_config::ServerConfig::new(
            self.address.clone(),
            self.database_url.clone(),
            self.tls_address.clone(),
            self.console_address.clone(),
        )?);

        let cookie_crypto = Arc::new(temps_core::CookieCrypto::new(&serve_config.auth_secret)?);
        let encryption_service = Arc::new(temps_core::EncryptionService::new(
            &serve_config.encryption_key,
        )?);

        info!(
            "Starting Temps proxy on {} and {}",
            self.address,
            self.tls_address
                .as_ref()
                .unwrap_or(&"no tls address".to_string())
        );

        debug!("Initializing database connection...");
        // Create tokio runtime for database connection since we need async for this
        let rt = tokio::runtime::Runtime::new()?;
        let db = rt.block_on(temps_database::establish_connection(&self.database_url))?;

        // Services are now available for use
        debug!("Cookie crypto and encryption services initialized");

        // Built before pingora takes over the process, on the runtime whose
        // worker threads keep driving whatever the gate spawns.
        let project_ip_gate = match ip_gate_builder {
            Some(build) => {
                let gate = build(db.clone(), rt.handle());
                debug!("proxy: enforcing project IP rules via a caller-supplied gate");
                gate
            }
            None => Arc::new(temps_core::OpenIpGate) as Arc<dyn temps_core::ProjectIpGate>,
        };

        // Start proxy server
        self.start_proxy_server(
            db,
            project_ip_gate,
            self.address.clone(),
            self.tls_address.clone(),
            self.console_address.clone(),
            cookie_crypto,
            encryption_service,
            serve_config.clone(),
            runtime_context,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn start_proxy_server(
        &self,
        db: Arc<DbConnection>,
        project_ip_gate: Arc<dyn temps_core::ProjectIpGate>,
        address: String,
        tls_address: Option<String>,
        console_address: Option<String>,
        cookie_crypto: Arc<CookieCrypto>,
        encryption_service: Arc<temps_core::EncryptionService>,
        config: Arc<ServerConfig>,
        runtime_context: Arc<temps_core::RuntimeContext>,
    ) -> anyhow::Result<()> {
        let data_dir = config.data_dir.clone();
        let console_address = console_address
            .ok_or_else(|| anyhow::anyhow!("Console address is required for proxy server"))?;

        // Create tokio runtime to fetch preview_domain from config service
        let rt = tokio::runtime::Runtime::new()?;

        // Fetch settings once: needed for `preview_domain`, ADR-017 version-skew
        // detection, and the ADR-018 on-demand TLS decision.
        let settings = rt.block_on(async {
            let config_service = temps_config::ConfigService::new(
                Arc::new(temps_config::ServerConfig::new(
                    address.clone(),
                    self.database_url.clone(),
                    tls_address.clone(),
                    Some(console_address.clone()),
                )?),
                db.clone(),
            );

            match config_service.get_settings().await {
                Ok(settings) => {
                    // ADR-017 Phase 3: version-skew detection. The console records
                    // its binary version on startup; warn if this proxy's binary
                    // differs (during a rolling upgrade the proxy may read tables a
                    // newer console has already migrated — schema-skew risk).
                    // Best-effort: this only logs and never aborts startup.
                    let proxy_version = crate::commands::upgrade::current_version_tag();
                    match compare_versions(&proxy_version, settings.console_version.as_deref()) {
                        SkewStatus::Match => {
                            info!(
                                proxy_version = %proxy_version,
                                "Version check: proxy and console both on {}",
                                proxy_version
                            );
                        }
                        SkewStatus::Skew { proxy, console } => {
                            warn!(
                                proxy_version = %proxy,
                                console_version = %console,
                                "VERSION SKEW: this proxy ({}) differs from the console ({}). \
                                 During a rolling upgrade the proxy may read tables a newer \
                                 console has already migrated — schema-skew risk. Upgrade the \
                                 proxy to match the console.",
                                proxy, console
                            );
                        }
                        SkewStatus::Unknown => {
                            debug!(
                                proxy_version = %proxy_version,
                                "Version check: console version not recorded yet; \
                                 skipping skew detection (proxy on {})",
                                proxy_version
                            );
                        }
                    }
                    Ok::<Option<temps_core::AppSettings>, anyhow::Error>(Some(settings))
                }
                Err(e) => {
                    warn!("Failed to fetch settings: {}, using defaults (preview_domain 'localhost', on-demand TLS disabled)", e);
                    Ok(None)
                }
            }
        })?;

        let preview_domain = Some(
            settings
                .as_ref()
                .map(|s| s.preview_domain.clone())
                .unwrap_or_else(|| "localhost".to_string()),
        );

        info!(
            "Starting proxy server with preview_domain: {:?}",
            preview_domain
        );

        // Create job queue for route table update notifications
        let (queue, _keep_alive_receiver): (Arc<dyn temps_core::JobQueue>, _) =
            temps_queue::BroadcastQueueService::create_job_queue_arc_with_receiver(1000);

        // Initialize route table with listener (preview_domain loaded from settings)
        let route_table = Arc::new(temps_proxy::CachedPeerTable::new_with_runtime_context(
            db.clone(),
            runtime_context.clone(),
        ));

        // ADR-018 on-demand TLS: build the certificate manager when enabled in
        // settings. Constructed after the route table exists (the gate's
        // cert-eligible route check reads it). `None` (the default, or when the
        // feature can't be safely enabled) keeps the TLS callback's existing
        // fail-fast behavior with no on-demand issuance — zero behavior change.
        let on_demand_cert_manager = match settings.as_ref() {
            Some(settings) => rt.block_on(
                crate::commands::serve::on_demand_cert::build_on_demand_cert_manager(
                    settings,
                    db.clone(),
                    encryption_service.clone(),
                    route_table.clone(),
                ),
            ),
            None => None,
        };

        let proxy_config = temps_proxy::ProxyConfig {
            address,
            console_address,
            tls_address,
            preview_domain,
            disable_https_redirect: self.disable_https_redirect,
            on_demand_cert_manager,
        };
        let listener = Arc::new(temps_routes::RouteTableListener::new(
            route_table.clone(),
            self.database_url.clone(),
            queue.clone(),
        ));

        // ── On-demand / scale-to-zero (ADR-017 Phase 2) ──
        //
        // In the split topology the proxy process — NOT the console — owns the
        // OnDemandManager: it watches request activity on the hot path, sleeps
        // idle containers, and wakes them on the first request. Containers run on
        // the proxy's host, so it talks to the local Docker socket directly.
        //
        // Best-effort: if Docker is unavailable, on-demand is disabled and the
        // proxy still serves everything else. The manager and the sleeping
        // callback MUST be wired before `start_listening()` below, so the
        // listener's initial load populates sleeping domains and on-demand
        // configs on the very first pass (mirroring `temps serve`).
        //
        // Cross-process wake correctness: `do_wake` publishes ForceRouteReload on
        // this process's queue AND fires raw PG `NOTIFY route_table_changes`. In
        // split mode the deploy/wake state lives in the DB; this proxy's own
        // RouteTableListener (started below) observes the NOTIFY, reloads the
        // table, and the sleeping callback calls `notify_route_reloaded()`. The
        // wake caller's `wait_for_route_reload` then unblocks — and the bounded
        // re-resolve loop is the correctness guarantee if the signal is missed.
        let on_demand_manager: Option<Arc<OnDemandManager>> = {
            let docker = rt.block_on(async {
                let docker = bollard::Docker::connect_with_defaults()
                    .map_err(|e| anyhow::anyhow!("Docker connect failed: {}", e))?;
                docker
                    .ping()
                    .await
                    .map_err(|e| anyhow::anyhow!("Docker ping failed: {}", e))?;
                Ok::<_, anyhow::Error>(docker)
            });
            match crate::commands::serve::proxy::optional_docker_feature(
                docker,
                "on-demand scale-to-zero wake for this proxy",
            ) {
                Some(docker) => {
                    let docker_runtime = temps_deployer::docker::DockerRuntime::new(
                        Arc::new(docker),
                        true,
                        "temps".to_string(),
                    );
                    let adapter = crate::commands::serve::proxy::ContainerLifecycleAdapter::new(
                        Arc::new(docker_runtime) as Arc<dyn temps_deployer::ContainerDeployer>,
                        runtime_context.clone(),
                    );
                    Some(Arc::new(OnDemandManager::new(
                        db.clone(),
                        Arc::new(adapter) as Arc<dyn ContainerLifecycle>,
                        queue.clone(),
                        // Standalone proxy on the control-plane host: locally
                        // deployed containers carry node_id=NULL (treated as
                        // local); remote-worker containers are skipped. Same
                        // semantics as `temps serve`. See issue #126.
                        None,
                    )))
                }
                None => None,
            }
        };

        // Register the sleeping-domain callback on the route table BEFORE the
        // listener's initial load, so sleeping domains + on-demand configs are
        // populated on the first pass and `notify_route_reloaded()` fires after
        // every subsequent reload (this is what `wait_for_route_reload` waits on
        // after a wake). Mirrors the wiring in `temps serve`.
        if let Some(ref on_demand_manager) = on_demand_manager {
            register_on_demand_sleeping_callback(&route_table, Arc::clone(on_demand_manager));

            // Background idle sweep: stops idle containers via the local Docker
            // socket. Only this process runs it in split mode — the console does
            // not instantiate an OnDemandManager.
            on_demand_manager.start_sweep_task(Duration::from_secs(60));
        }

        // Start route table listener
        // Keep `listener` itself alive on the stack — only pass a clone into
        // start_listening(). RouteTableListener's Drop aborts its background
        // recv task, so consuming the only Arc reference here would abort the
        // task the instant this block_on call returns: the "Started listening"
        // log would fire, but the loop would never actually process a single
        // NOTIFY. Mirrors the pattern already used for `project_listener` below
        // and for `route_table_listener` in `serve/mod.rs`.
        info!("Starting route table listener...");
        let listener_clone = listener.clone();
        rt.block_on(async move { listener_clone.start_listening().await })?;

        // Start project change listener
        // Keep the listener alive on the stack so its Drop doesn't abort the background task
        info!("Starting project change listener...");
        let project_listener = temps_routes::ProjectChangeListener::new(
            self.database_url.clone(),
            route_table.clone(),
            queue.clone(),
        );
        rt.block_on(async {
            if let Err(e) = project_listener.start_listening().await {
                tracing::error!("Project change listener failed: {}", e);
            }
        });

        // Start the in-process route reload subscriber. Reloads the route table
        // on Job::ForceRouteReload published over the shared queue.
        //
        // NOTE: In this standalone `temps proxy` command the deploy pipeline
        // runs in a *separate* control-plane process with its own queue, so
        // ForceRouteReload events never reach this subscriber — the PG
        // LISTEN/NOTIFY path (ProjectChangeListener / RouteTableListener above)
        // remains the route-reload mechanism here. The deterministic in-process
        // path only applies to the single-binary `temps serve` mode where the
        // control plane and proxy share one queue. We still wire the subscriber
        // for consistency and so it works if this process ever also runs the
        // deploy pipeline. Kept alive on the stack so its Drop doesn't abort the task.
        info!("Starting route reload subscriber...");
        let route_reload_subscriber =
            temps_routes::RouteReloadSubscriber::new(route_table.clone(), queue.clone());
        // start() calls tokio::spawn, so it must run inside the runtime context.
        rt.block_on(async { route_reload_subscriber.start() });

        // Wire the admin gate from boot-time config (env vars take precedence,
        // else the `settings` DB row). This 404s requests to admin/management
        // routes that fall outside the allowlist before they ever reach the
        // console listener — the same network-layer enforcement `temps serve`
        // gives the in-process proxy. We only need the resolved handle here, not
        // the full `AdminGateService` (which exists to back the console's
        // persist API). `new` fails CLOSED on a DB error, so a broken settings
        // row refuses to boot rather than opening the gate.
        //
        // Live admin-gate edits made through the console's API swap only the
        // console's in-process handle, so this separate process subscribes to
        // the Postgres `settings_change` channel and reloads the allowlist
        // itself. Without that, this proxy would enforce its boot-time config
        // forever: a newly saved allowlist would go unenforced here, and a
        // cleared one would keep 404ing hosts that should now fall through to
        // the console.
        let admin_gate_handle = match rt.block_on(
            crate::commands::serve::admin_gate_service::AdminGateService::new(
                db.clone(),
                &config.admin_allowed_ips,
                &config.admin_allowed_hosts,
                config.admin_trust_forwarded_for,
            ),
        ) {
            Ok((service, handle)) => {
                // `start_settings_listener` calls `tokio::spawn`, so it must run
                // inside the runtime context. `rt` lives on this stack for the
                // duration of the blocking Pingora server below, which is what
                // keeps the task alive (same pattern as the route listeners).
                let service = Arc::new(service);
                let database_url = self.database_url.clone();
                rt.block_on(async { service.start_settings_listener(database_url) });
                Some(handle)
            }
            Err(e) => {
                return Err(anyhow::anyhow!(
                    "Failed to initialize admin gate: {}. Refusing to start the proxy with \
                     an unresolved gate. Fix the `settings` row or set TEMPS_ADMIN_ALLOWED_IPS / \
                     TEMPS_ADMIN_ALLOWED_HOSTS.",
                    e
                ));
            }
        };

        let shutdown_signal = Box::new(CtrlCShutdownSignal::new(
            Duration::from_secs(30),
            db.clone(),
            data_dir.clone(),
        )) as Box<dyn ProxyShutdownSignal>;

        match temps_proxy::setup_proxy_server(
            db,
            proxy_config,
            cookie_crypto,
            encryption_service,
            route_table,
            shutdown_signal,
            config.clone(),
            on_demand_manager, // wired in split mode (ADR-017 Phase 2); None if Docker unavailable
            admin_gate_handle,
            // This standalone `temps proxy` process never loads a console or
            // its plugins — there is nothing here to register an alternative
            // resolver.
            Arc::new(temps_core::FixedRetentionResolver),
            // Supplied by the caller when the embedding binary knows how to
            // build one; `temps_core::OpenIpGate` (allow everything) otherwise,
            // which is what the plain `temps proxy` entrypoint passes.
            project_ip_gate,
        ) {
            Ok(_) => {
                info!("Proxy server exited");
                Ok(())
            }
            Err(e) => {
                error!("Failed to start proxy server: {}", e);
                Err(anyhow::anyhow!("Failed to start proxy server: {}", e))
            }
        }
    }
}

/// Build the on-demand sleeping-domain callback for the standalone proxy's
/// [`OnDemandManager`].
///
/// On every route-table load the callback rebuilds the manager's sleeping-domain
/// index and on-demand config set from the freshly loaded routes, then fires
/// `notify_route_reloaded()` to release any request parked in the wake path
/// (`wait_for_route_reload`). This is the split-mode equivalent of the wiring in
/// `temps serve`. Split out from registration so the closure can be unit-tested
/// directly, without standing up a Pingora server or firing it through a real
/// route-table load.
fn build_on_demand_sleeping_callback(
    manager: Arc<OnDemandManager>,
) -> temps_routes::route_table::OnSleepingCallback {
    Arc::new(move |entries, on_demand_configs| {
        manager.clear_sleeping_domains();
        for entry in entries {
            manager.register_sleeping_domain(
                entry.domain.clone(),
                temps_proxy::on_demand::SleepingEnvironmentInfo {
                    environment_id: entry.environment_id,
                    project_id: entry.project_id,
                    deployment_id: entry.deployment_id,
                    wake_timeout_seconds: entry.wake_timeout_seconds,
                },
            );
        }
        for config in on_demand_configs {
            manager.register_on_demand_environment(
                config.environment_id,
                config.idle_timeout_seconds,
                config.wake_timeout_seconds,
            );
        }
        manager.notify_route_reloaded();
    })
}

/// Install the on-demand sleeping-domain callback on `route_table`. Must be
/// registered BEFORE the route-table listener's first load so the initial load
/// populates on-demand state.
fn register_on_demand_sleeping_callback(
    route_table: &Arc<temps_proxy::CachedPeerTable>,
    manager: Arc<OnDemandManager>,
) {
    route_table.set_on_sleeping_callback(build_on_demand_sleeping_callback(manager));
}

#[cfg(test)]
mod on_demand_callback_tests {
    use super::*;
    use async_trait::async_trait;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use temps_proxy::on_demand::OnDemandError;
    use temps_routes::route_table::{OnDemandConfigEntry, SleepingEnvironmentEntry};

    /// A `ContainerLifecycle` that does nothing — these tests exercise the
    /// callback's bookkeeping (registering sleeping domains + configs), not
    /// container I/O, so the lifecycle is never actually called.
    struct NoopLifecycle;

    #[async_trait]
    impl ContainerLifecycle for NoopLifecycle {
        async fn start_container(&self, _id: &str) -> Result<(), OnDemandError> {
            Ok(())
        }
        async fn stop_container(&self, _id: &str) -> Result<(), OnDemandError> {
            Ok(())
        }
        async fn is_container_healthy(&self, _id: &str) -> Result<bool, OnDemandError> {
            Ok(true)
        }
    }

    fn test_manager() -> Arc<OnDemandManager> {
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        let (queue, _rx): (Arc<dyn temps_core::JobQueue>, _) =
            temps_queue::BroadcastQueueService::create_job_queue_arc_with_receiver(16);
        Arc::new(OnDemandManager::new(
            db,
            Arc::new(NoopLifecycle) as Arc<dyn ContainerLifecycle>,
            queue,
            None,
        ))
    }

    fn sleeping_entry(domain: &str, env_id: i32) -> SleepingEnvironmentEntry {
        SleepingEnvironmentEntry {
            domain: domain.to_string(),
            environment_id: env_id,
            project_id: 7,
            deployment_id: 100 + env_id,
            wake_timeout_seconds: 45,
        }
    }

    #[test]
    fn callback_registers_sleeping_domains_for_wake_lookup() {
        // This is the heart of split-mode wake-on-request: after a route load,
        // the proxy must be able to map an incoming hostname to a sleeping
        // environment so the request hot path can wake it.
        let manager = test_manager();
        let callback = build_on_demand_sleeping_callback(manager.clone());

        callback(
            vec![sleeping_entry("app.example.com", 1)],
            vec![OnDemandConfigEntry {
                environment_id: 1,
                idle_timeout_seconds: 300,
                wake_timeout_seconds: 45,
            }],
        );

        let found = manager
            .get_sleeping_environment("app.example.com")
            .expect("registered sleeping domain must be resolvable");
        assert_eq!(found.environment_id, 1);
        assert_eq!(found.project_id, 7);
        assert_eq!(found.deployment_id, 101);
        assert_eq!(found.wake_timeout_seconds, 45);

        // A domain that was never registered must not resolve.
        assert!(manager
            .get_sleeping_environment("unknown.example.com")
            .is_none());
    }

    #[test]
    fn callback_clears_stale_sleeping_domains_on_reload() {
        // Each load rebuilds the index from scratch: an environment that woke up
        // (and so is absent from the next load's sleeping set) must no longer be
        // resolvable as sleeping, or the proxy would try to wake an already-awake
        // env on the next request.
        let manager = test_manager();
        let callback = build_on_demand_sleeping_callback(manager.clone());

        // First load: two sleeping envs.
        callback(
            vec![
                sleeping_entry("a.example.com", 1),
                sleeping_entry("b.example.com", 2),
            ],
            vec![],
        );
        assert!(manager.get_sleeping_environment("a.example.com").is_some());
        assert!(manager.get_sleeping_environment("b.example.com").is_some());

        // Second load: only `b` is still sleeping (`a` woke up). `a` must be gone.
        callback(vec![sleeping_entry("b.example.com", 2)], vec![]);
        assert!(
            manager.get_sleeping_environment("a.example.com").is_none(),
            "a domain absent from the latest load must be cleared"
        );
        assert!(manager.get_sleeping_environment("b.example.com").is_some());
    }
}

#[cfg(test)]
mod skew_tests {
    use super::{compare_versions, SkewStatus};

    #[test]
    fn test_compare_versions_match() {
        assert_eq!(
            compare_versions("v0.1.0", Some("v0.1.0")),
            SkewStatus::Match
        );
    }

    #[test]
    fn test_compare_versions_skew() {
        assert_eq!(
            compare_versions("v0.1.0", Some("v0.2.0")),
            SkewStatus::Skew {
                proxy: "v0.1.0".into(),
                console: "v0.2.0".into()
            }
        );
    }

    #[test]
    fn test_compare_versions_absent_is_unknown() {
        assert_eq!(compare_versions("v0.1.0", None), SkewStatus::Unknown);
    }

    #[test]
    fn test_compare_versions_empty_console_is_unknown() {
        assert_eq!(compare_versions("v0.1.0", Some("")), SkewStatus::Unknown);
        assert_eq!(compare_versions("v0.1.0", Some("   ")), SkewStatus::Unknown);
    }

    #[test]
    fn test_compare_versions_trims_whitespace_match() {
        assert_eq!(
            compare_versions("v0.1.0", Some(" v0.1.0 ")),
            SkewStatus::Match
        );
    }

    #[test]
    fn test_compare_versions_garbage_does_not_panic() {
        // Total: arbitrary bytes must not panic, must classify as Skew/Match.
        assert_eq!(
            compare_versions("????", Some("v0.1.0")),
            SkewStatus::Skew {
                proxy: "????".into(),
                console: "v0.1.0".into()
            }
        );
        assert_eq!(compare_versions("", Some("")), SkewStatus::Unknown);
    }
}
