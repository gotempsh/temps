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

    /// Report a container ready only once its application is actually accepting
    /// connections — not merely once Docker reports `Running`.
    ///
    /// A `Running` container whose process hasn't yet bound its port would, on a
    /// scale-to-zero wake, get a request proxied to it before it can serve,
    /// producing a spurious upstream-connect 503 on the first request. So after
    /// confirming `Running`, we TCP-probe a mapped host port (short timeout,
    /// treated as "not ready yet" on failure so `do_wake`'s loop keeps polling).
    ///
    /// **Scope:** the probe targets `127.0.0.1:{host_port}` — the loopback
    /// address the local node publishes container ports on. Containers running on
    /// *remote* worker nodes are not reachable on this node's loopback; that
    /// (along with the fact that the local deployer can't even
    /// `get_container_info` a remote container) is handled by the multi-node wake
    /// work tracked separately. For the local single-node case this is correct.
    /// We probe the **lowest** published host port deterministically (Docker
    /// reports ports as an unordered map, so `.first()` would be unstable for a
    /// container that publishes more than one). Containers with no published port
    /// fall back to the `Running` check — there's nothing to probe.
    async fn is_container_healthy(&self, container_id: &str) -> Result<bool, OnDemandError> {
        let info = self
            .deployer
            .get_container_info(container_id)
            .await
            .map_err(|e| OnDemandError::ContainerOperation {
                container_id: container_id.to_string(),
                reason: e.to_string(),
            })?;

        if info.status != temps_deployer::ContainerStatus::Running {
            return Ok(false);
        }

        // No published port → nothing to probe; trust the Running status.
        // Pick deterministically (lowest host port) — Docker's port map has no
        // defined iteration order, so `.first()` could vary between polls.
        let Some(port) = info.ports.iter().map(|p| p.host_port).min() else {
            return Ok(true);
        };

        // Probe the mapped host port. A refused/timed-out connection means the
        // app inside hasn't bound its port yet — report not-ready so the wake
        // loop keeps polling rather than completing prematurely.
        let addr = format!("127.0.0.1:{}", port);
        match tokio::time::timeout(
            Duration::from_secs(2),
            tokio::net::TcpStream::connect(&addr),
        )
        .await
        {
            Ok(Ok(_stream)) => Ok(true),
            Ok(Err(e)) => {
                tracing::debug!(
                    container_id = %container_id,
                    addr = %addr,
                    error = %e,
                    "Readiness probe connect failed; container not ready yet"
                );
                Ok(false)
            }
            Err(_) => {
                tracing::debug!(
                    container_id = %container_id,
                    addr = %addr,
                    "Readiness probe timed out; container not ready yet"
                );
                Ok(false)
            }
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

    // Fetch settings once: we need `preview_domain` for routing AND the full
    // `AppSettings` to decide whether to wire ADR-018 on-demand TLS.
    let settings = rt.block_on(async {
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
            Ok(settings) => Ok::<Option<temps_core::AppSettings>, anyhow::Error>(Some(settings)),
            Err(e) => {
                warn!(
                    "Failed to fetch settings: {}, using defaults (preview_domain 'localhost', \
                     on-demand TLS disabled)",
                    e
                );
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

    // ADR-018 on-demand TLS: build the certificate manager when enabled in
    // settings. `None` (the default, or when the feature can't be safely
    // enabled) keeps the TLS callback's existing fail-fast behavior with no
    // on-demand issuance — zero behavior change.
    let on_demand_cert_manager = match settings.as_ref() {
        Some(settings) => rt.block_on(super::on_demand_cert::build_on_demand_cert_manager(
            settings,
            db.clone(),
            encryption_service.clone(),
            route_table.clone(),
        )),
        None => None,
    };

    let proxy_config = temps_proxy::ProxyConfig {
        address,
        console_address,
        tls_address,
        preview_domain,
        disable_https_redirect,
        on_demand_cert_manager,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use temps_deployer::{
        ContainerInfo, ContainerStats, ContainerStatus, DeployRequest, DeployResult, DeployerError,
        PortMapping, Protocol,
    };

    /// Minimal `ContainerDeployer` that returns a canned `ContainerInfo` from
    /// `get_container_info`. Every other method is unreachable in these tests —
    /// `is_container_healthy` only calls `get_container_info`.
    struct MockDeployer {
        info: ContainerInfo,
    }

    fn container_info(status: ContainerStatus, ports: Vec<u16>) -> ContainerInfo {
        ContainerInfo {
            container_id: "c1".to_string(),
            container_name: "app".to_string(),
            image_name: "app:latest".to_string(),
            status,
            created_at: chrono::Utc::now(),
            ports: ports
                .into_iter()
                .map(|host_port| PortMapping {
                    host_port,
                    container_port: 3000,
                    protocol: Protocol::Tcp,
                })
                .collect(),
            environment_vars: HashMap::new(),
            restart_count: None,
            labels: HashMap::new(),
            exit_code: None,
            exit_reason: None,
            oom_killed: None,
            error_message: None,
            finished_at: None,
            started_at: None,
            cpu_limit_cores: None,
        }
    }

    #[async_trait]
    impl ContainerDeployer for MockDeployer {
        async fn deploy_container(
            &self,
            _request: DeployRequest,
        ) -> Result<DeployResult, DeployerError> {
            unimplemented!("not used by is_container_healthy tests")
        }
        async fn start_container(&self, _container_id: &str) -> Result<(), DeployerError> {
            unimplemented!()
        }
        async fn stop_container(&self, _container_id: &str) -> Result<(), DeployerError> {
            unimplemented!()
        }
        async fn pause_container(&self, _container_id: &str) -> Result<(), DeployerError> {
            unimplemented!()
        }
        async fn resume_container(&self, _container_id: &str) -> Result<(), DeployerError> {
            unimplemented!()
        }
        async fn remove_container(&self, _container_id: &str) -> Result<(), DeployerError> {
            unimplemented!()
        }
        async fn get_container_info(
            &self,
            _container_id: &str,
        ) -> Result<ContainerInfo, DeployerError> {
            Ok(self.info.clone())
        }
        async fn get_container_stats(
            &self,
            _container_id: &str,
        ) -> Result<ContainerStats, DeployerError> {
            unimplemented!()
        }
        async fn list_containers(&self) -> Result<Vec<ContainerInfo>, DeployerError> {
            unimplemented!()
        }
        async fn get_container_logs(&self, _container_id: &str) -> Result<String, DeployerError> {
            unimplemented!()
        }
        async fn stream_container_logs(
            &self,
            _container_id: &str,
        ) -> Result<Box<dyn futures::Stream<Item = String> + Unpin + Send>, DeployerError> {
            unimplemented!()
        }
    }

    fn adapter_for(info: ContainerInfo) -> ContainerLifecycleAdapter {
        ContainerLifecycleAdapter::new(Arc::new(MockDeployer { info }))
    }

    #[tokio::test]
    async fn test_not_running_is_not_healthy() {
        let adapter = adapter_for(container_info(ContainerStatus::Created, vec![12345]));
        assert!(!adapter.is_container_healthy("c1").await.unwrap());
    }

    #[tokio::test]
    async fn test_running_no_ports_falls_back_to_running() {
        // Nothing to probe → trust the Running status.
        let adapter = adapter_for(container_info(ContainerStatus::Running, vec![]));
        assert!(adapter.is_container_healthy("c1").await.unwrap());
    }

    #[tokio::test]
    async fn test_running_port_listening_is_healthy() {
        // Bind a real listener so the readiness probe's TCP connect succeeds.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let adapter = adapter_for(container_info(ContainerStatus::Running, vec![port]));
        assert!(
            adapter.is_container_healthy("c1").await.unwrap(),
            "Running container with a listening port must be healthy"
        );
    }

    #[tokio::test]
    async fn test_running_port_closed_is_not_healthy() {
        // Bind then drop the listener so the port is closed → connect refused →
        // the app isn't ready yet, so the container must NOT be reported healthy.
        let port = {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            listener.local_addr().unwrap().port()
        };

        let adapter = adapter_for(container_info(ContainerStatus::Running, vec![port]));
        assert!(
            !adapter.is_container_healthy("c1").await.unwrap(),
            "Running container whose port refuses connections must not be healthy"
        );
    }

    #[tokio::test]
    async fn test_probes_lowest_port_deterministically() {
        // Bind two listeners, keep only the LOWER-numbered one alive (close the
        // higher). The probe must target the lowest published port, so the
        // container is healthy iff the lowest port is the listening one — proving
        // selection is by value, not by the (unordered) report order.
        let a = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let b = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let pa = a.local_addr().unwrap().port();
        let pb = b.local_addr().unwrap().port();
        let (lo, hi, lo_listener, hi_listener) = if pa < pb {
            (pa, pb, a, b)
        } else {
            (pb, pa, b, a)
        };
        drop(hi_listener); // higher port now closed; lower port still listening
        let _keep = lo_listener; // keep the lower port bound for the probe

        // Report ports high-then-low to prove order-independence.
        let adapter = adapter_for(container_info(ContainerStatus::Running, vec![hi, lo]));
        assert!(
            adapter.is_container_healthy("c1").await.unwrap(),
            "probe must target the lowest published port (the listening one)"
        );
    }
}
