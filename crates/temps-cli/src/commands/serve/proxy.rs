use async_trait::async_trait;
use std::sync::Arc;
use std::time::Duration;
use temps_config::ServerConfig;
use temps_core::CookieCrypto;
use temps_database::DbConnection;
use temps_deployer::ContainerDeployer;
use temps_proxy::on_demand::{ContainerLifecycle, OnDemandError, OnDemandManager};
use temps_proxy::ProxyShutdownSignal;
use tracing::{info, warn};

use super::shutdown::CtrlCShutdownSignal;

/// Adapter bridging `temps_deployer::ContainerDeployer` to `temps_proxy::on_demand::ContainerLifecycle`.
pub(crate) struct ContainerLifecycleAdapter {
    deployer: Arc<dyn ContainerDeployer>,
}

impl ContainerLifecycleAdapter {
    pub fn new(deployer: Arc<dyn ContainerDeployer>) -> Self {
        Self { deployer }
    }
}

#[async_trait]
impl ContainerLifecycle for ContainerLifecycleAdapter {
    async fn start_container(&self, container_id: &str) -> Result<(), OnDemandError> {
        self.deployer
            .start_container(container_id)
            .await
            .map_err(|e| OnDemandError::ContainerOperation {
                container_id: container_id.to_string(),
                reason: e.to_string(),
            })
    }

    async fn stop_container(&self, container_id: &str) -> Result<(), OnDemandError> {
        self.deployer
            .stop_container(container_id)
            .await
            .map_err(|e| OnDemandError::ContainerOperation {
                container_id: container_id.to_string(),
                reason: e.to_string(),
            })
    }

    async fn is_container_healthy(&self, container_id: &str) -> Result<bool, OnDemandError> {
        match self.deployer.get_container_info(container_id).await {
            Ok(info) => Ok(info.status == temps_deployer::ContainerStatus::Running),
            Err(e) => Err(OnDemandError::ContainerOperation {
                container_id: container_id.to_string(),
                reason: e.to_string(),
            }),
        }
    }
}

/// Initialize and start the proxy server
#[allow(clippy::too_many_arguments)]
pub fn start_proxy_server(
    db: Arc<DbConnection>,
    address: String,
    tls_address: Option<String>,
    cookie_crypto: Arc<CookieCrypto>,
    encryption_service: Arc<temps_core::EncryptionService>,
    database_url: String,
    route_table: Arc<temps_proxy::CachedPeerTable>,
    config: Arc<ServerConfig>,
    disable_https_redirect: bool,
    on_demand_manager: Option<Arc<OnDemandManager>>,
    admin_gate: Option<temps_core::admin_gate::AdminGateHandle>,
) -> anyhow::Result<()> {
    let console_address = config.console_address.clone();
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
                warn!(
                    "Failed to fetch preview_domain from settings: {}, using default 'localhost'",
                    e
                );
                Ok(Some("localhost".to_string()))
            }
        }
    })?;

    let proxy_config = temps_proxy::ProxyConfig {
        address,
        console_address,
        tls_address,
        preview_domain,
        disable_https_redirect,
    };

    info!(
        "Starting proxy server with preview_domain: {:?}",
        proxy_config.preview_domain
    );

    if disable_https_redirect {
        warn!("HTTPS redirect is disabled - HTTP requests will NOT be redirected to HTTPS");
    }

    let shutdown_signal = Box::new(CtrlCShutdownSignal::new(
        Duration::from_secs(30),
        db.clone(),
        config.data_dir.clone(),
    )) as Box<dyn ProxyShutdownSignal>;

    match temps_proxy::setup_proxy_server(
        db,
        proxy_config,
        cookie_crypto,
        encryption_service,
        route_table,
        shutdown_signal,
        config.clone(),
        on_demand_manager,
        admin_gate,
    ) {
        Ok(_) => {
            info!("Proxy server exited");
            Ok(())
        }
        Err(e) => {
            tracing::error!("Failed to start proxy server: {}", e);
            Err(anyhow::anyhow!("Failed to start proxy server: {}", e))
        }
    }
}
