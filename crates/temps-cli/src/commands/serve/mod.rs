pub mod console;
mod proxy;
mod shutdown;

use clap::Args;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::{debug, info, warn};

pub use console::start_console_api;
pub use proxy::start_proxy_server;

#[derive(Args)]
pub struct ServeCommand {
    /// Address to bind the server to
    #[arg(long, default_value = "127.0.0.1:3000", env = "TEMPS_ADDRESS")]
    pub address: String,

    /// TLS address to bind the server to
    #[arg(long, env = "TEMPS_TLS_ADDRESS")]
    pub tls_address: Option<String>,

    /// Database connection URL
    #[arg(long, env = "TEMPS_DATABASE_URL")]
    pub database_url: String,

    /// Data directory for storing configuration and runtime files
    #[arg(long, env = "TEMPS_DATA_DIR")]
    pub data_dir: Option<PathBuf>,

    /// Console/Admin address (defaults to random port on localhost)
    #[arg(long, env = "TEMPS_CONSOLE_ADDRESS")]
    pub console_address: Option<String>,

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
}

impl ServeCommand {
    pub fn execute(self) -> anyhow::Result<()> {
        // Install the rustls crypto provider once at startup. Both temps-domains
        // and check-if-email-exists try to install it themselves — calling it here
        // first satisfies the library's internal Once guard and prevents panics.
        check_if_email_exists::initialize_crypto_provider();

        // Set screenshot provider from CLI flag (takes precedence over env var)
        // This allows: temps serve --screenshot-provider=noop
        if let Some(ref provider) = self.screenshot_provider {
            std::env::set_var("TEMPS_SCREENSHOT_PROVIDER", provider);
            debug!("Screenshot provider set to '{}' from CLI flag", provider);
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
        // Create tokio runtime for database connection since we need async for this
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

        let route_table_listener = Arc::new(temps_routes::RouteTableListener::new(
            route_table.clone(),
            self.database_url.clone(),
            queue.clone(),
        ));

        let rt = tokio::runtime::Runtime::new()?;
        // Start the route table listener (block_on to ensure initial load completes)
        let route_table_listener_clone = route_table_listener.clone();
        rt.block_on(async move {
            if let Err(e) = route_table_listener_clone.start_listening().await {
                tracing::error!("Route table listener failed: {}", e);
            }
        });

        // Start the project change listener
        // Keep the listener alive on the stack so its Drop doesn't abort the background task
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

        let on_demand_manager: Option<Arc<temps_proxy::on_demand::OnDemandManager>> =
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
                ))
            });

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

        // Start console API server in background (non-blocking).
        // The proxy does NOT wait for the console to be ready. This ensures that
        // deployed applications remain reachable even if console initialization
        // fails (e.g. Docker check, GeoIP validation, plugin init). Console API
        // requests will get connection-refused until the console finishes starting,
        // but that is far better than all proxied traffic being down.
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
        };
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

        // Monitor console readiness in a background thread so we can log it,
        // but do NOT block proxy startup on it.
        std::thread::spawn(move || {
            let rt = match tokio::runtime::Runtime::new() {
                Ok(rt) => rt,
                Err(e) => {
                    tracing::error!("Failed to create runtime for console monitor: {}", e);
                    return;
                }
            };
            match rt.block_on(ready_rx) {
                Ok(()) => {
                    info!("✅ Console API is ready");
                }
                Err(_) => {
                    tracing::error!(
                        "❌ Console API failed to become ready — check error logs above"
                    );
                }
            }
        });

        info!("Starting proxy server (console API initializing in background)...");

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
        )
    }
}
