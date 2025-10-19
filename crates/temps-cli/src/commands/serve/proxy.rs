use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use temps_core::CookieCrypto;
use temps_database::DbConnection;
use temps_proxy::ProxyShutdownSignal;
use tracing::{error, info, warn};

use super::shutdown::CtrlCShutdownSignal;

/// Initialize and start the proxy server
pub fn start_proxy_server(
    db: Arc<DbConnection>,
    address: String,
    tls_address: Option<String>,
    console_address: String,
    cookie_crypto: Arc<CookieCrypto>,
    encryption_service: Arc<temps_core::EncryptionService>,
    data_dir: PathBuf,
    database_url: String,
    route_table: Arc<temps_proxy::CachedPeerTable>,
) -> anyhow::Result<()> {
    // Create tokio runtime to fetch preview_domain from config service
    let rt = tokio::runtime::Runtime::new()?;

    // Get preview_domain from settings
    let preview_domain = rt.block_on(async {
        let config_service = temps_config::ConfigService::new(
            Arc::new(temps_config::ServerConfig::new(
                address.clone(),
                database_url.clone(),
                tls_address.clone(),
                Some(console_address.clone()),
            )?),
            db.clone(),
        );

        match config_service.get_settings().await {
            Ok(settings) => Ok::<Option<String>, anyhow::Error>(Some(settings.preview_domain)),
            Err(e) => {
                warn!("Failed to fetch preview_domain from settings: {}, using default 'localhost'", e);
                Ok(Some("localhost".to_string()))
            }
        }
    })?;

    let proxy_config = temps_proxy::ProxyConfig {
        address,
        console_address,
        tls_address,
        preview_domain,
    };

    info!("Starting proxy server with preview_domain: {:?}", proxy_config.preview_domain);

    // Note: Route table is now created and listener is started in serve/mod.rs
    // The same instance is shared between console API and proxy server

    let shutdown_signal = Box::new(CtrlCShutdownSignal::new(
        Duration::from_secs(30),
        db.clone(),
        data_dir.clone()
    )) as Box<dyn ProxyShutdownSignal>;

    match temps_proxy::setup_proxy_server(db, proxy_config, cookie_crypto, encryption_service, route_table, shutdown_signal) {
        Ok(_) => {
            info!("Proxy server exited");
            Ok(())
        },
        Err(e) => {
            error!("Failed to start proxy server: {}", e);
            Err(anyhow::anyhow!("Failed to start proxy server: {}", e))
        }
    }
}