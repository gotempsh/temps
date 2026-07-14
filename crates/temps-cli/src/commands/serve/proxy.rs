use async_trait::async_trait;
use std::sync::Arc;
use std::time::Duration;
use temps_config::ServerConfig;
use temps_core::CookieCrypto;
use temps_database::DbConnection;
use temps_deployer::ContainerDeployer;
use temps_proxy::on_demand::{ContainerLifecycle, OnDemandError, OnDemandManager};
use temps_proxy::on_demand_cert::EnqueueOutcome;
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

    /// Report a container ready only once its application is actually answering
    /// HTTP — not merely once Docker reports `Running`.
    ///
    /// A `Running` container whose process hasn't yet bound its port would, on a
    /// scale-to-zero wake, get a request proxied to it before it can serve,
    /// producing a spurious upstream-connect 503 on the first request. The
    /// readiness probe lives in `temps_deployer::readiness`; it issues an HTTP
    /// GET (Docker `Running` + a real HTTP response) rather than a bare TCP
    /// connect — a TCP connect is defeated by Docker's userland proxy, which
    /// accepts the connection before the app inside has bound its port (so a
    /// TCP handshake would falsely report "ready"). See that module for details.
    ///
    /// `do_wake` runs its own outer poll loop, so this is a single-shot check:
    /// `Ok(false)` means "not ready yet, keep polling". A container in a
    /// terminal state (`Exited`/`Dead`) is reported not-healthy here too — the
    /// wake loop's own timeout then surfaces the failure.
    async fn is_container_healthy(&self, container_id: &str) -> Result<bool, OnDemandError> {
        use temps_deployer::readiness::{check_accepting_requests, ReadinessCheck};

        // 2s per-request timeout matches the historical inline probe.
        let check = check_accepting_requests(&self.deployer, container_id, Duration::from_secs(2))
            .await
            .map_err(|e| OnDemandError::ContainerOperation {
                container_id: container_id.to_string(),
                reason: e.to_string(),
            })?;

        Ok(matches!(check, ReadinessCheck::Ready))
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
    retention_resolver: Arc<dyn temps_core::RetentionResolver>,
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

    // ADR-018 eager cert pre-provisioning: wire the on-demand cert manager into
    // the route table so every `load_routes()` call — triggered by new
    // deployments — immediately enqueues TLS issuance for each cert-eligible
    // hostname. The existing gate checks (dedup, backoff, rate-limit, zone) inside
    // `try_enqueue` make this idempotent: only genuinely new or retryable hosts
    // produce issuance jobs. This eliminates the `ERR_TLS_HANDSHAKE` on the first
    // request to a freshly-deployed app.
    if let Some(ref cert_manager) = on_demand_cert_manager {
        let cert_manager_for_callback = cert_manager.clone();
        route_table.set_on_cert_eligible_callback(std::sync::Arc::new(
            move |hostnames: Vec<String>| {
                let cert_manager = cert_manager_for_callback.clone();
                Box::pin(async move {
                    let mut enqueued = 0u32;
                    for hostname in &hostnames {
                        if let EnqueueOutcome::Enqueued = cert_manager.try_enqueue(hostname, None)
                        {
                            enqueued += 1;
                        }
                    }
                    if enqueued > 0 {
                        tracing::info!(
                            enqueued,
                            total = hostnames.len(),
                            "on-demand TLS: eagerly pre-provisioning {} cert(s) for new/updated routes",
                            enqueued
                        );
                    }
                }) as std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
            },
        ));

        // One-time immediate pass for cert-eligible routes already in the table
        // (populated by the initial load that runs before this code executes).
        let initial_hosts = route_table.cert_eligible_hosts();
        if !initial_hosts.is_empty() {
            let mut enqueued = 0u32;
            for hostname in &initial_hosts {
                if let EnqueueOutcome::Enqueued = cert_manager.try_enqueue(hostname, None) {
                    enqueued += 1;
                }
            }
            if enqueued > 0 {
                tracing::info!(
                    enqueued,
                    total = initial_hosts.len(),
                    "on-demand TLS: eagerly pre-provisioning {} cert(s) for existing routes at startup",
                    enqueued
                );
            }
        }
    }

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
        retention_resolver,
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

    /// Build a `ContainerInfo` pointed at a test listener. The readiness probe
    /// resolves its URL via `DeploymentMode::build_container_url`, which yields
    /// `(container_name, container_port)` in Docker mode and `("127.0.0.1",
    /// host_port)` in baremetal mode. Using `container_name = "127.0.0.1"` and
    /// `container_port == host_port` makes both modes resolve to
    /// `http://127.0.0.1:{port}/`, so these tests don't depend on the ambient
    /// `DEPLOYMENT_MODE`.
    fn container_info(status: ContainerStatus, ports: Vec<u16>) -> ContainerInfo {
        ContainerInfo {
            container_id: "c1".to_string(),
            container_name: "127.0.0.1".to_string(),
            image_name: "app:latest".to_string(),
            status,
            created_at: chrono::Utc::now(),
            ports: ports
                .into_iter()
                .map(|host_port| PortMapping {
                    host_port,
                    container_port: host_port,
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

    /// Spawn a minimal HTTP/1.1 server on a loopback port that answers `200 OK`
    /// to any request, returning the bound port. The readiness probe issues a
    /// real HTTP GET (a bare TCP listener would be reported not-ready, which is
    /// the whole point of the HTTP probe), so tests that need a "ready" port
    /// must actually speak HTTP.
    async fn spawn_http_ok() -> u16 {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut buf = [0u8; 1024];
                    let _ = sock.read(&mut buf).await;
                    let _ = sock
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                        )
                        .await;
                    let _ = sock.flush().await;
                });
            }
        });
        port
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
    async fn test_running_port_serving_http_is_healthy() {
        // An HTTP server on the mapped port → the probe gets a 200 → healthy.
        let port = spawn_http_ok().await;
        let adapter = adapter_for(container_info(ContainerStatus::Running, vec![port]));
        assert!(
            adapter.is_container_healthy("c1").await.unwrap(),
            "Running container that answers HTTP must be healthy"
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
    async fn test_running_tcp_open_but_no_http_is_not_healthy() {
        // The docker-proxy false-positive: a raw TCP listener that never speaks
        // HTTP. A TCP-only probe would call this "ready"; the HTTP probe must
        // not (the connect succeeds but no HTTP response arrives within the
        // per-request timeout).
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        // Accept connections but never respond, holding the socket open.
        tokio::spawn(async move {
            loop {
                let Ok((sock, _)) = listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_secs(30)).await;
                    drop(sock);
                });
            }
        });

        let adapter = adapter_for(container_info(ContainerStatus::Running, vec![port]));
        assert!(
            !adapter.is_container_healthy("c1").await.unwrap(),
            "TCP-open-but-silent container must NOT be reported healthy (docker-proxy false-positive)"
        );
    }

    #[tokio::test]
    async fn test_probes_lowest_port_deterministically() {
        // Serve HTTP only on the LOWER-numbered port; the higher is closed. The
        // probe must target the lowest published port, so the container is
        // healthy iff the lowest port is the serving one — proving selection is
        // by value, not by the (unordered) report order.
        let lo_port = spawn_http_ok().await;
        let hi_port = {
            // A bound-then-dropped higher port: pick something above lo_port and
            // ensure it's closed. Bind to 0 until we get a port > lo_port.
            let mut p;
            loop {
                let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                p = l.local_addr().unwrap().port();
                if p > lo_port {
                    break; // dropped here → closed
                }
            }
            p
        };

        // Report ports high-then-low to prove order-independence.
        let adapter = adapter_for(container_info(
            ContainerStatus::Running,
            vec![hi_port, lo_port],
        ));
        assert!(
            adapter.is_container_healthy("c1").await.unwrap(),
            "probe must target the lowest published port (the HTTP-serving one)"
        );
    }
}
