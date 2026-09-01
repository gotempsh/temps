// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

pub(crate) mod admin_gate;
mod admin_gate_handler;
pub(crate) mod admin_gate_service;
pub mod console;
pub(crate) mod on_demand_cert;
pub(crate) mod proxy;
pub(crate) mod self_update;
mod shutdown;

use clap::{Args, ValueEnum};
use std::path::PathBuf;
use std::sync::Arc;
use tracing::{debug, info, warn};

pub use console::start_console_api;
pub use proxy::start_proxy_server;

const POST_MIGRATION_INDEX_INITIAL_RETRY: std::time::Duration = std::time::Duration::from_secs(5);
const POST_MIGRATION_INDEX_MAX_RETRY: std::time::Duration = std::time::Duration::from_secs(300);

/// Bound on how long single-binary proxy startup waits for console plugin
/// initialization (specifically, for a licensed plugin to claim
/// `project_ip_gate_slot`) before starting to serve traffic anyway. See the
/// comment at the call site for why this exists and why it is bounded
/// rather than an unconditional wait.
const PROJECT_IP_GATE_STARTUP_GRACE: std::time::Duration = std::time::Duration::from_secs(10);

fn next_post_migration_index_retry(current: std::time::Duration) -> std::time::Duration {
    current
        .saturating_mul(2)
        .min(POST_MIGRATION_INDEX_MAX_RETRY)
}

/// Which halves of the control plane this `temps serve` process runs.
///
/// The default (`All`) is the single-binary control plane that has always
/// existed: the Pingora proxy (:80/:443) and the console (API + web + plugins +
/// background workers) run together in one process. `Console` runs only the
/// console half, leaving the proxy to a separate `temps proxy` process, so the
/// console can be upgraded/restarted without dropping production traffic on
/// :80/:443 (see ADR-017). There is intentionally no `Proxy` role here — the
/// proxy half is the existing standalone `temps proxy` command, not a role of
/// `serve`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum ServeRole {
    /// Run both the proxy and the console in one process (the default,
    /// backwards-compatible single-binary control plane).
    #[default]
    All,
    /// Run only the console (API + web + plugins + background workers); do NOT
    /// bind :80/:443 and do NOT start the on-demand wake manager (those belong
    /// to the proxy process). Pair with a separate `temps proxy`.
    Console,
}

#[derive(Args)]
pub struct ServeCommand {
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

    /// Console/Admin address (defaults to random port on localhost).
    ///
    /// When `--console-admin-address` is unset, this address serves both public
    /// ingest endpoints (event tracking, AI gateway, etc.) and admin routes.
    /// When the admin address is set, this listener only serves public routes.
    #[arg(long, env = "TEMPS_CONSOLE_ADDRESS")]
    pub console_address: Option<String>,

    /// Optional dedicated address for admin/management routes. When set, the
    /// `--console-address` listener only serves public ingest routes and the
    /// admin/UI surface (auth, projects, settings, dashboard) binds here.
    ///
    /// Combine with `--admin-allowed-ips` / `--admin-allowed-hosts` to add a
    /// defense-in-depth allowlist on top of the network-layer isolation.
    #[arg(long, env = "TEMPS_CONSOLE_ADMIN_ADDRESS")]
    pub console_admin_address: Option<String>,

    /// Forbid applying release updates from the console, permanently for the
    /// lifetime of this process.
    ///
    /// The console also has a Settings toggle for the same thing, but that one
    /// lives in the database and can be switched back on by anyone who can
    /// write settings. This flag cannot: it is set at launch, so an operator
    /// who keeps upgrades under configuration management (or policy) can rule
    /// the API path out entirely. The update banner still appears and still
    /// shows the manual command — only the "Update now" action is refused.
    #[arg(long)]
    pub disable_self_update: bool,

    /// Screenshot provider to use: "local" (headless Chrome), "remote", or "noop" (disabled)
    /// Use "noop" on servers without Chrome installed to skip screenshot functionality
    #[arg(long, env = "TEMPS_SCREENSHOT_PROVIDER", value_parser = ["local", "remote", "noop", "disabled", "none"])]
    pub screenshot_provider: Option<String>,

    /// Additional template YAML files to load (can be specified multiple times)
    /// Templates are merged with the bundled defaults; validation errors will prevent startup
    #[arg(long = "templates", env = "TEMPS_ADDITIONAL_TEMPLATES")]
    pub additional_templates: Vec<PathBuf>,

    /// Disable HTTP-to-HTTPS redirect (useful for local development without TLS)
    #[arg(long, env = "TEMPS_DISABLE_HTTPS_REDIRECT")]
    pub disable_https_redirect: bool,

    /// Private/WireGuard IP address of this control plane node.
    /// Worker nodes use this address to reach services (databases, etc.) on the control plane.
    #[arg(long, env = "TEMPS_PRIVATE_ADDRESS")]
    pub private_address: Option<String>,

    /// Which halves of the control plane to run in this process.
    ///
    /// `all` (default) runs the proxy (:80/:443) and the console together —
    /// the single-binary control plane. `console` runs only the console so it
    /// can be upgraded independently of the proxy; pair it with a separate
    /// `temps proxy` process (ADR-017 split topology). In `console` mode a
    /// stable `--console-address` / `TEMPS_CONSOLE_ADDRESS` is REQUIRED so the
    /// sibling proxy has a fixed address to forward console traffic to.
    #[arg(long, value_enum, default_value_t = ServeRole::All, env = "TEMPS_ROLE")]
    pub role: ServeRole,
}

impl ServeCommand {
    /// Run `temps serve` with the OSS-only plugin set.
    pub fn execute(self) -> anyhow::Result<()> {
        self.execute_with_extra_plugins(Vec::new())
    }

    /// Run `temps serve` with additional plugins registered alongside the
    /// OSS ones. This is the entrypoint used by EE-bundled binaries (per
    /// ADR 0001 §"Extension points exposed by OSS"); pass a fresh
    /// `vec![Box::new(TeamsPlugin::new()), ...]` and the extra plugins are
    /// registered just before `initialize_plugins`, so they observe the
    /// full OSS service registry.
    pub fn execute_with_extra_plugins(
        self,
        extra_plugins: Vec<Box<dyn temps_core::plugin::TempsPlugin>>,
    ) -> anyhow::Result<()> {
        // Install the rustls crypto provider once at startup, before any
        // dependency (e.g. temps-domains) constructs a rustls client.
        crate::install_crypto_provider();

        // Set screenshot provider from CLI flag (takes precedence over env var)
        // This allows: temps serve --screenshot-provider=noop
        if let Some(ref provider) = self.screenshot_provider {
            std::env::set_var("TEMPS_SCREENSHOT_PROVIDER", provider);
            debug!("Screenshot provider set to '{}' from CLI flag", provider);
        }

        // Bridge the optional CLI flag into the env var so ServerConfig::new
        // picks it up regardless of which path the operator used.
        if let Some(ref admin) = self.console_admin_address {
            std::env::set_var("TEMPS_CONSOLE_ADMIN_ADDRESS", admin);
        }

        // Same bridge for the data directory. `ServerConfig::new` resolves it
        // from TEMPS_DATA_DIR or falls back to ~/.temps, and never saw this
        // flag — so `--data-dir /srv/temps` was silently ignored and the
        // server wrote its encryption key, auth secret, logs and plugin data
        // to the home directory instead. Two instances started with different
        // `--data-dir` values would quietly share one directory.
        if let Some(ref dir) = self.data_dir {
            std::env::set_var("TEMPS_DATA_DIR", dir);
            debug!("Data directory set to '{}' from CLI flag", dir.display());
        }

        // In split (`--role=console`) mode the console must bind a STABLE,
        // known address: the sibling `temps proxy` forwards console/UI traffic
        // to it (proxy `ProxyConfig.console_address`). The default is a random
        // localhost port (`ServerConfig::get_random_console_address`), which a
        // separate proxy process cannot discover — so require it explicitly and
        // fail fast with a clear message rather than binding an ephemeral port
        // the proxy will never find. (In `all` mode the proxy is in-process and
        // reads the resolved address directly, so the random default is fine.)
        if self.role == ServeRole::Console && self.console_address.is_none() {
            anyhow::bail!(
                "`temps serve --role=console` requires a stable console address.\n\n\
                 Set --console-address (or TEMPS_CONSOLE_ADDRESS), e.g. \
                 `--console-address 127.0.0.1:8081`, and point the sibling \
                 `temps proxy --console-address <same>` at it. Without it the \
                 console would bind a random port the proxy process cannot find."
            );
        }

        let serve_config = Arc::new(temps_config::ServerConfig::new(
            self.address.clone(),
            self.database_url.clone(),
            self.tls_address.clone(),
            self.console_address.clone(),
        )?);
        let encryption_service = Arc::new(temps_core::EncryptionService::new(
            &serve_config.encryption_key,
        )?);

        let cookie_crypto = Arc::new(temps_core::CookieCrypto::new(&serve_config.auth_secret)?);

        debug!("Initializing database connection...");
        // THE long-lived runtime for this process. It runs the startup
        // `block_on`s just below, and later the index build, the backfill, the
        // listeners and the console (`rt.spawn` further down). It stays alive
        // until `serve` returns: under `--role=all` that is when pingora stops
        // blocking this thread, and under `--role=console` when the console
        // future it is blocking on finishes.
        //
        // It is deliberately built HERE, *before* the pool, rather than as a
        // second runtime after startup: the pool has to be created on the
        // runtime that will keep driving it. sqlx spawns a pool-maintenance
        // task when the pool is constructed, and every socket the pool opens
        // (`min_connections`, plus the connection migrations run on) registers
        // with the creating runtime's IO driver. A startup-only runtime would
        // leave both stranded -- the maintenance task unpolled and the pooled
        // sockets bound to a driver nothing runs -- and the first query issued
        // from the main runtime would hang forever.
        let rt = tokio::runtime::Runtime::new()?;
        let db = rt.block_on(temps_database::establish_connection(&self.database_url))?;

        // Update private address setting from CLI flag
        if let Some(ref private_address) = self.private_address {
            info!("Private address set to: {}", private_address);
            let db_for_settings = db.clone();
            let private_addr = private_address.clone();
            let serve_config_for_addr = serve_config.clone();
            rt.block_on(async move {
                let config_service =
                    temps_config::ConfigService::new(serve_config_for_addr, db_for_settings);
                if let Err(e) = config_service
                    .update_setting_field(|settings| {
                        settings.multi_node.private_address = Some(private_addr);
                    })
                    .await
                {
                    tracing::error!("Failed to update private address setting: {}", e);
                }
            });
        }

        // ADR-017 Phase 3: record the console's binary version so a sibling
        // standalone `temps proxy` can WARN on version skew during a rolling
        // upgrade. This runs for BOTH ServeRole::All and ServeRole::Console —
        // the role branches happen later, so this single write covers both.
        // Best-effort: skew detection is advisory and must never fail startup.
        {
            let db_for_version = db.clone();
            let serve_config_for_version = serve_config.clone();
            let console_version = crate::commands::upgrade::current_version_tag();
            rt.block_on(async move {
                let config_service =
                    temps_config::ConfigService::new(serve_config_for_version, db_for_version);
                if let Err(e) = config_service
                    .update_setting_field(|settings| {
                        settings.console_version = Some(console_version);
                    })
                    .await
                {
                    tracing::warn!("Failed to record console version for skew detection: {}", e);
                }
            });
        }

        info!(
            "Starting Temps server on {} and {}",
            self.address,
            self.tls_address
                .as_ref()
                .unwrap_or(&"no tls address".to_string())
        );

        // Services are now available for use
        debug!("Cookie crypto and encryption services initialized");

        // Create the shared job queue FIRST — it is used by route table listeners
        // (to publish RouteTableUpdated) and by the console API (for all other jobs).
        let (queue, _keep_alive_receiver): (Arc<dyn temps_core::JobQueue>, _) =
            temps_queue::BroadcastQueueService::create_job_queue_arc_with_receiver(1000);

        // Create shared route table instance (used by both console API and proxy)
        let route_table = Arc::new(temps_proxy::CachedPeerTable::new(db.clone()));

        // ADR-012-lite: every successful route_table reload also
        // reconciles internal `<env>.<project>.temps.local` A records,
        // so the L7 routes and the internal DNS zone share one trigger
        // and one source of truth.
        {
            let publisher = Arc::new(temps_dns::DeploymentDnsPublisher::new(
                db.clone(),
                Arc::new(temps_dns::DnsRegistry::new(db.clone())),
            ));
            route_table.set_on_reload_callback(Arc::new(move || {
                let publisher = publisher.clone();
                Box::pin(async move {
                    if let Err(e) = publisher.reconcile_all().await {
                        tracing::warn!(
                            error = %e,
                            "deployment DNS publisher failed; routes are live but \
                             internal *.temps.local records may be stale"
                        );
                    }
                })
            }));
        }

        // Construct the route-reload machinery now, but DON'T start it yet.
        // We must register the on-demand sleeping-domain callback on the shared
        // route table BEFORE the listener's initial load runs, so the very first
        // load populates sleeping domains and on-demand configs. That callback
        // depends on `on_demand_manager`, which depends on the Docker handle
        // resolved below — so the actual starts happen after that block.
        let route_table_listener = Arc::new(temps_routes::RouteTableListener::new(
            route_table.clone(),
            self.database_url.clone(),
            queue.clone(),
        ));
        // Keep the project listener alive on the stack so its Drop doesn't abort
        // the background task.
        let project_listener = temps_routes::ProjectChangeListener::new(
            self.database_url.clone(),
            route_table.clone(),
            queue.clone(),
        );
        // The in-process route reload subscriber: the deterministic, single-node
        // route-reload path. The deploy pipeline publishes Job::ForceRouteReload
        // on this same shared queue after writing current_deployment_id, and this
        // subscriber reloads the route table directly. Unlike the PG LISTEN/NOTIFY
        // path, it has no database connection that can silently wedge between
        // deployments, so a freshly deployed environment is guaranteed to become
        // routable without a manual reload. (NOTIFY is still used to reach remote
        // worker nodes that don't share this queue.) Keep it alive on the stack.
        let route_reload_subscriber =
            temps_routes::RouteReloadSubscriber::new(route_table.clone(), queue.clone());

        // Run non-transactional indexes and the TimescaleDB backfill on this
        // long-lived runtime, detached. Index creation retries with capped
        // backoff until the retention query has its supporting index; the
        // idempotent backfill remains best-effort and never gates proxy bind.
        {
            let index_db = db.clone();
            rt.spawn(async move {
                let mut retry_delay = POST_MIGRATION_INDEX_INITIAL_RETRY;
                loop {
                    match temps_database::run_post_migration_indexes(index_db.as_ref()).await {
                        Ok(()) => break,
                        Err(e) => {
                            tracing::warn!(
                                "Post-migration index build failed; retrying in {:?}: {}",
                                retry_delay,
                                e
                            );
                            tokio::time::sleep(retry_delay).await;
                            retry_delay = next_post_migration_index_retry(retry_delay);
                        }
                    }
                }
            });

            let backfill_db = db.clone();
            rt.spawn(async move {
                if let Err(e) =
                    temps_database::run_post_migration_backfill(backfill_db.as_ref()).await
                {
                    tracing::warn!(
                        "Post-migration backfill failed (refresh policy will catch up): {}",
                        e
                    );
                }
            });
        }

        // Update notifier: shortly after startup, then at the configured
        // interval (two hours by default), check GitHub
        // for a newer release on this install's channel (stable vs beta is
        // inferred from the running version tag). Hits land in this shared
        // slot, which the console registers as a service so the settings API
        // (`GET /settings/update-status`) can drive the web-console upgrade
        // banner. Detached and best-effort — network failures are
        // debug-logged and it never gates startup. Spawned before the role
        // branch so it covers both `--role=all` (this runtime outlives the
        // blocking proxy) and `--role=console` (the console block_on below
        // runs on this same runtime).
        let update_status = Arc::new(temps_core::UpdateStatusSlot::new());
        let update_check_interval = crate::commands::upgrade::configured_update_check_interval();
        rt.spawn(crate::commands::upgrade::update_notifier_loop(
            update_status.clone(),
            update_check_interval,
            Arc::new(temps_config::ConfigService::new(
                serve_config.clone(),
                db.clone(),
            )),
        ));

        // Companion to the notifier above: the notifier says a release exists,
        // this applies it when an admin asks. Constructed here (not in the
        // console) so it resolves the journal of a previous update attempt
        // exactly once per process, before any request can observe it.
        //
        // In split topology (ADR-017) only the CONSOLE process restarts. The
        // sibling `temps proxy` keeps serving :80/:443 on the binary it already
        // exec'd, so the operator has to restart it separately to converge —
        // stated up front rather than discovered as version skew later.
        let self_update_caveat = (self.role == ServeRole::Console).then(|| {
            "This process runs the console only (ADR-017 split topology). The separate \
             `temps proxy` service keeps serving traffic on the binary it started with — \
             restart it too once the console is back to finish the upgrade."
                .to_string()
        });
        let self_updater = Arc::new(self_update::BinarySelfUpdater::new(
            serve_config.data_dir.clone(),
            self.disable_self_update,
            self_update_caveat,
            update_status.clone(),
            self.database_url.clone(),
        ));

        // Connect to Docker once and share the handle between:
        //   1. OnDemandManager (wake-on-request scale-to-zero)
        //   2. Preview gateway reconciler (workspace preview routing)
        //
        // Both are non-fatal — if Docker is unavailable we log and continue.
        // The proxy server (80/443) MUST come up regardless.
        let docker_handle: Option<Arc<bollard::Docker>> = {
            let docker_rt = tokio::runtime::Runtime::new()?;
            match docker_rt.block_on(async {
                let docker = bollard::Docker::connect_with_defaults()
                    .map_err(|e| anyhow::anyhow!("Docker connect failed: {}", e))?;
                docker
                    .ping()
                    .await
                    .map_err(|e| anyhow::anyhow!("Docker ping failed: {}", e))?;
                Ok::<_, anyhow::Error>(docker)
            }) {
                Ok(docker) => Some(Arc::new(docker)),
                Err(e) => {
                    warn!(
                        "Docker not available — on-demand scale-to-zero and workspace \
                         preview gateway will be disabled: {}",
                        e
                    );
                    None
                }
            }
        };

        // The on-demand wake manager is a PROXY-side concern: it watches request
        // activity, sleeps idle containers, and wakes them on the first request
        // through the proxy. In split (`--role=console`) mode the proxy runs in a
        // separate `temps proxy` process that owns its own OnDemandManager
        // (ADR-017 Phase 2), so the console process must NOT construct one — it
        // has no proxy request path to drive it and would start a second, useless
        // idle-sweep task competing to stop containers. Skip it here in console
        // mode; the console's wake API reaches the proxy via PG NOTIFY instead.
        let on_demand_manager: Option<Arc<temps_proxy::on_demand::OnDemandManager>> =
            if self.role == ServeRole::Console {
                None
            } else {
                docker_handle.as_ref().map(|docker| {
                    let docker_runtime = temps_deployer::docker::DockerRuntime::new(
                        docker.clone(),
                        true,
                        "temps".to_string(),
                    );
                    let adapter = proxy::ContainerLifecycleAdapter::new(
                        Arc::new(docker_runtime) as Arc<dyn temps_deployer::ContainerDeployer>
                    );
                    Arc::new(temps_proxy::on_demand::OnDemandManager::new(
                        db.clone(),
                        Arc::new(adapter) as Arc<dyn temps_proxy::on_demand::ContainerLifecycle>,
                        queue.clone(),
                        // Control plane has no self node row; its locally-deployed
                        // containers carry node_id=NULL, which is treated as local.
                        // Remote-worker containers (node_id != NULL) are skipped so a
                        // multi-node deployment's wake/sleep no longer reverts on a
                        // failed local start. See issue #126.
                        None,
                    ))
                })
            };

        // Register the on-demand sleeping-domain callback on the shared route
        // table BEFORE starting the listener, so the listener's (background)
        // initial load populates sleeping domains and on-demand configs on the
        // first pass. This replaces the old duplicate `load_routes()` that used
        // to run inside `setup_proxy_server` purely because the callback was
        // registered too late.
        if let Some(ref on_demand_manager) = on_demand_manager {
            let on_demand_for_callback = Arc::clone(on_demand_manager);
            route_table.set_on_sleeping_callback(Arc::new(move |entries, on_demand_configs| {
                on_demand_for_callback.clear_sleeping_domains();
                for entry in entries {
                    on_demand_for_callback.register_sleeping_domain(
                        entry.domain.clone(),
                        temps_proxy::on_demand::SleepingEnvironmentInfo {
                            environment_id: entry.environment_id,
                            project_id: entry.project_id,
                            deployment_id: entry.deployment_id,
                            wake_timeout_seconds: entry.wake_timeout_seconds,
                        },
                    );
                }
                // Register on-demand configs so the idle sweep can track awake environments
                for config in on_demand_configs {
                    on_demand_for_callback.register_on_demand_environment(
                        config.environment_id,
                        config.idle_timeout_seconds,
                        config.wake_timeout_seconds,
                    );
                }
                // Signal any requests waiting for routes after a wake
                on_demand_for_callback.notify_route_reloaded();
            }));

            // Start background idle sweep (checks every 60 seconds)
            on_demand_manager.start_sweep_task(std::time::Duration::from_secs(60));
        }

        // NOW start the route-reload machinery. `start_listening` subscribes to
        // PG NOTIFY synchronously and spawns the initial load in the background,
        // so none of these block the proxy bind. The sleeping callback above is
        // already registered, so the first load populates on-demand state.
        let route_table_listener_clone = route_table_listener.clone();
        rt.block_on(async move {
            if let Err(e) = route_table_listener_clone.start_listening().await {
                tracing::error!("Route table listener failed: {}", e);
            }
        });
        rt.block_on(async {
            if let Err(e) = project_listener.start_listening().await {
                tracing::error!("Project change listener failed: {}", e);
            }
        });
        // start() calls tokio::spawn, so it must run inside the runtime context.
        rt.block_on(async { route_reload_subscriber.start() });

        // Kick off preview gateway reconciliation in the background. This pulls
        // the image (if needed), creates the shared sandbox network, and starts
        // the gateway container. It MUST NOT block proxy startup — workspace
        // previews are a non-critical subsystem.
        if let Some(docker) = docker_handle.clone() {
            let data_dir = self
                .data_dir
                .clone()
                .or_else(|| std::env::var("TEMPS_DATA_DIR").ok().map(PathBuf::from))
                .unwrap_or_else(|| {
                    dirs::home_dir()
                        .unwrap_or_else(|| PathBuf::from("."))
                        .join(".temps")
                });
            temps_agents::preview_gateway::spawn_reconcile(&rt, docker, db.clone(), data_dir);
        }

        // Build the admin-gate handle up-front so both the console listener
        // and the Pingora proxy see the same source of truth. Env precedence
        // is resolved here; the DB is consulted on first read inside the
        // service (which the console code constructs separately).
        let (admin_gate_service, admin_gate_handle) = rt
            .block_on(admin_gate_service::AdminGateService::new(
                db.clone(),
                &serve_config.admin_allowed_ips,
                &serve_config.admin_allowed_hosts,
                serve_config.admin_trust_forwarded_for,
            ))
            .map_err(|e| anyhow::anyhow!("Failed to initialize admin gate: {}", e))?;
        {
            let snapshot = admin_gate_handle.current();
            info!(
                source = ?snapshot.source,
                allowed_ips = ?snapshot.allowed_nets.iter().map(|n| n.to_string()).collect::<Vec<_>>(),
                allowed_hosts = ?snapshot.allowed_hosts,
                trust_forwarded_for = snapshot.trust_forwarded_for,
                is_noop = snapshot.is_noop(),
                "Admin gate initialized"
            );
        }

        // Converge on out-of-process gate edits here too. A single-binary
        // `temps serve` swaps this handle in-process on save, so the listener
        // is redundant for the common case — but in an HA deployment with
        // several consoles against one database, replica B would otherwise keep
        // enforcing its boot-time allowlist after an operator tightened the
        // gate on replica A. `AdminGateService` is `Clone` and the handle is an
        // `Arc<ArcSwap<_>>`, so this clone writes to the same live handle.
        {
            let listener_service = Arc::new(admin_gate_service.clone());
            let database_url = self.database_url.clone();
            rt.block_on(async { listener_service.start_settings_listener(database_url) });
        }

        // Shared retention-resolver slot: constructed ONCE here (before either
        // the console or the proxy bootstraps begin) so both see the same
        // object. The console's ProxyPlugin looks this up via the service
        // registry and swaps in a plugin-provided resolver once one registers;
        // the proxy (started separately below, own isolated plugin context)
        // gets a direct clone as a function parameter since it can never see
        // anything registered in the console's registry. See ADR 0017
        // follow-up: register_services runs in plugin-registration order and
        // the proxy is a wholly separate bootstrap, so neither side can reach
        // the other's service registry — this slot is the one thing shared
        // across both.
        //
        // **Security guardrail — shared-slot pattern:** this cross-context
        // sharing is permissible for `RetentionResolverSlot` only because its
        // sole effect is a per-row metadata value (`retention_days`) with no
        // bearing on request routing, authorization, or connection handling.
        // Do NOT add new objects to this pattern without an explicit security
        // review: any object that influences auth, TLS/cert issuance, IP
        // blocklists, or rate limiting MUST NOT be shared across plugin
        // contexts this way.
        let retention_resolver_slot = Arc::new(temps_core::RetentionResolverSlot::new_default());

        // See the field doc on `ConsoleApiParams::project_ip_gate_slot` —
        // same shared-slot mechanism and construction site as
        // retention_resolver_slot immediately above, flagged for security
        // review as an explicit exception rather than a second precedent.
        let project_ip_gate_slot = Arc::new(temps_core::ProjectIpGateSlot::new_default());

        // Build the console params once; both roles consume them.
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let params = console::ConsoleApiParams {
            db: db.clone(),
            config: serve_config.clone(),
            cookie_crypto: cookie_crypto.clone(),
            encryption_service: encryption_service.clone(),
            route_table: route_table.clone(),
            queue: queue.clone(),
            ready_signal: Some(ready_tx),
            additional_templates: self.additional_templates.clone(),
            on_demand_waker: on_demand_manager
                .clone()
                .map(|m| m as Arc<dyn temps_core::OnDemandWaker>),
            extra_plugins,
            admin_gate_service: Some(admin_gate_service),
            admin_gate_handle: Some(admin_gate_handle.clone()),
            retention_resolver_slot: retention_resolver_slot.clone(),
            project_ip_gate_slot: project_ip_gate_slot.clone(),
            update_status,
            self_updater,
        };

        if self.role == ServeRole::Console {
            // Split topology (ADR-017): run ONLY the console. The proxy lives in a
            // separate `temps proxy` process, so we never bind :80/:443 here. The
            // console future runs to completion on the main runtime and IS the
            // blocking foreground task (mirroring how the proxy blocks in `all`
            // mode). This is what makes the console independently restartable:
            // killing/upgrading this process leaves the sibling proxy — and all
            // production traffic on :80/:443 — untouched.
            //
            // The route-table listeners started above still run so this process's
            // CachedPeerTable stays warm (used by the console's own routing
            // views), but the authoritative table that serves production traffic
            // is the one in the proxy process, kept fresh over PG NOTIFY.
            info!(
                "Starting in split mode: console only (proxy runs as a separate \
                 `temps proxy` process). Health: GET {}/readyz",
                serve_config.console_address
            );
            return rt.block_on(async move {
                start_console_api(params).await.map_err(|e| {
                    tracing::error!("❌ Console API failed: {}", e);
                    tracing::error!("Error details: {:?}", e);
                    e
                })
            });
        }

        // Single-binary mode (`--role=all`, the default): start the console in
        // the background (non-blocking) and let the proxy block the main thread.
        //
        // The proxy does NOT wait for the console to be ready. This ensures that
        // deployed applications remain reachable even if console initialization
        // fails (e.g. Docker check, GeoIP validation, plugin init). Console API
        // requests will get connection-refused until the console finishes starting,
        // but that is far better than all proxied traffic being down.
        rt.spawn(async move {
            match start_console_api(params).await {
                Ok(()) => {
                    info!("Console API server exited normally");
                }
                Err(e) => {
                    tracing::error!("❌ Console API failed to start: {}", e);
                    tracing::error!("Error details: {:?}", e);
                    tracing::error!(
                        "The console management UI will not be available. \
                         Proxied traffic to deployed applications is NOT affected."
                    );
                }
            }
        });

        // Wait for the console's plugin two-phase init to finish before the
        // proxy starts serving traffic — this is deliberately NOT the same
        // thing as waiting for the console to be fully healthy (routers,
        // middleware, admin gate, listener bind), which could take much
        // longer or hang on something unrelated (Docker check, GeoIP
        // validation, etc.) — exactly the "proxied traffic goes down because
        // of a console problem" failure mode the comment above exists to
        // avoid.
        //
        // The reason this wait exists at all: `project_ip_gate_slot` (see
        // the security guardrail comment above) starts as `OpenIpGate`
        // (allow everything) and is only claimed once plugin two-phase init
        // completes — `ProxyPlugin::initialize` claims it from whatever
        // `Arc<dyn ProjectIpGate>` an EE plugin registered, and that runs
        // inside `initialize_plugins()`, nothing later. `console.rs` fires
        // `ready_signal` (see its call site, right after "All plugins
        // initialized successfully") at exactly that point — not at the end
        // of `start_console_api` like it used to. Before that change
        // (P1 security finding on PR #725), this wait was tied to the FULL
        // console being ready, so every IP-restricted project was reachable
        // by any client for however long the rest of console startup took,
        // on every single boot. Now the wait resolves as soon as the one
        // thing it actually depends on is done — deterministically, since
        // plugin registration+init is in-memory service wiring with no
        // listener bind, no HTTP router construction, and (bar a
        // pathological plugin) no long-running I/O. The timeout below is a
        // backstop against a genuinely hung plugin `initialize()`, not the
        // expected path.
        // `tokio::time::timeout(..)` must be constructed *inside* the
        // runtime context `block_on` establishes, not as a bare argument
        // evaluated on this plain sync thread before `block_on` starts --
        // it eagerly builds a `Sleep` that registers with the current
        // runtime's timer driver via `Handle::current()`, which panics
        // ("there is no reactor running") if called with no ambient
        // runtime. Wrapping it in an `async` block defers construction
        // until `block_on` is already polling it.
        match rt
            .block_on(async { tokio::time::timeout(PROJECT_IP_GATE_STARTUP_GRACE, ready_rx).await })
        {
            Ok(Ok(())) => {
                info!(
                    "✅ Plugin init complete — any project IP gate a licensed plugin \
                     installed is in place before the proxy starts serving"
                );
            }
            Ok(Err(_)) => {
                tracing::error!(
                    "❌ Console plugin initialization failed — check error logs above. \
                     Starting the proxy anyway (proxied traffic to deployed applications \
                     is not held hostage by a console failure), but note: any \
                     project-scoped IP restriction will NOT be enforced until the console \
                     problem is resolved and the process is restarted."
                );
            }
            Err(_) => {
                tracing::warn!(
                    "⏳ Console plugin initialization did not complete within {:?} — this \
                     should not happen in normal operation (it means a plugin's own \
                     initialize() is hung, not merely that console startup is slow). \
                     Starting the proxy anyway rather than blocking indefinitely. Any \
                     project-scoped IP restriction will not be enforced until plugin \
                     initialization finishes; this is a bounded, logged exposure window, \
                     not the unbounded one this wait exists to close.",
                    PROJECT_IP_GATE_STARTUP_GRACE
                );
            }
        }

        info!("Starting proxy server...");

        // Start proxy server (this will block until shutdown)
        start_proxy_server(
            db,
            self.address.clone(),
            self.tls_address.clone(),
            cookie_crypto.clone(),
            encryption_service.clone(),
            self.database_url.clone(),
            route_table,
            serve_config.clone(),
            self.disable_https_redirect,
            on_demand_manager,
            Some(admin_gate_handle),
            retention_resolver_slot as Arc<dyn temps_core::RetentionResolver>,
            project_ip_gate_slot as Arc<dyn temps_core::ProjectIpGate>,
        )
    }
}

#[cfg(test)]
mod post_migration_tests {
    use super::*;

    #[test]
    fn index_retry_backoff_grows_and_caps() {
        let mut delay = POST_MIGRATION_INDEX_INITIAL_RETRY;
        for expected in [10, 20, 40, 80, 160, 300, 300] {
            delay = next_post_migration_index_retry(delay);
            assert_eq!(delay, std::time::Duration::from_secs(expected));
        }
    }
}
