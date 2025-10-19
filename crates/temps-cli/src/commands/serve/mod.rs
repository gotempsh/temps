pub mod console;
mod proxy;
mod shutdown;

use clap::Args;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::{debug, info};

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
}

impl ServeCommand {
    pub fn execute(self) -> anyhow::Result<()> {
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

        info!(
            "Starting Temps server on {} and {}",
            self.address,
            self.tls_address
                .as_ref()
                .unwrap_or(&"no tls address".to_string())
        );

        // Services are now available for use
        debug!("Cookie crypto and encryption services initialized");

        // Create shared route table instance (used by both console API and proxy)
        let route_table = Arc::new(temps_proxy::CachedPeerTable::new(db.clone()));
        let listener = Arc::new(temps_routes::RouteTableListener::new(
            route_table.clone(),
            self.database_url.clone(),
        ));

        let rt = tokio::runtime::Runtime::new()?;
        // Start the route table listener
        rt.spawn(async move {
            if let Err(e) = listener.start_listening().await {
                tracing::error!("Route table listener failed: {}", e);
            }
        });

        // Create a channel to wait for console API to be ready
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();

        // Start console API server in background with error handling
        let db_clone = db.clone();
        let serve_config_clone = serve_config.clone();
        let cookie_crypto_clone = cookie_crypto.clone();
        let encryption_service_clone = encryption_service.clone();
        let route_table_clone = route_table.clone();

        rt.spawn(async move {
            if let Err(e) = start_console_api(
                db_clone,
                serve_config_clone,
                cookie_crypto_clone,
                encryption_service_clone,
                route_table_clone,
                Some(ready_tx),
            ).await {
                tracing::error!("Failed to start console API server: {}", e);
                tracing::error!("Console API server will not be available");
            }
        });

        // Wait for console API to be ready before starting proxy
        info!("Waiting for console API to be ready...");
        if let Err(err) = rt.block_on(ready_rx) {
            tracing::error!("Console API failed to start properly: {}", err);
            return Err(anyhow::anyhow!("Console API failed to start: {}", err));
        }
        info!("Console API is ready, starting proxy server...");

        // Start proxy server (this will block until shutdown)
        start_proxy_server(
            db,
            self.address.clone(),
            self.tls_address.clone(),
            serve_config.console_address.clone(),
            cookie_crypto.clone(),
            encryption_service.clone(),
            serve_config.data_dir.clone(),
            self.database_url.clone(),
            route_table,
        )
    }
}
