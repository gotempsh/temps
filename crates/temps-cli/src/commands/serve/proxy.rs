// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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

#[derive(Debug, thiserror::Error)]
enum ProxyDnsBootstrapError {
    #[error("Cannot prepare the app network for proxy DNS: {0}")]
    Network(#[from] temps_deployer::DeployerError),
    #[error("Cannot start proxy DNS: app-network gateway is unavailable")]
    MissingGateway,
    #[error("Proxy DNS resolver failed to start on {0}:53")]
    Resolver(std::net::IpAddr),
    #[error("Cannot discover Docker for proxy DNS: {0}")]
    Docker(#[from] bollard::errors::Error),
    #[error("Cannot publish proxy DNS readiness: shared resolver slot is poisoned")]
    PoisonedSlot,
    #[error("Proxy DNS app-network preparation timed out after 10 seconds")]
    NetworkTimeout,
}

const DNS_RETRY_INTERVAL: Duration = Duration::from_secs(5);
const DNS_MAX_CONSECUTIVE_FAILURES: u32 = 12;

fn publish_dns_gateway(
    slot: &temps_dns::OverlayDnsSlot,
    gateway: Option<std::net::IpAddr>,
) -> Result<(), ProxyDnsBootstrapError> {
    *slot
        .write()
        .map_err(|_| ProxyDnsBootstrapError::PoisonedSlot)? = gateway;
    Ok(())
}

pub(crate) fn spawn_control_plane_dns_bootstrap(
    runtime: &tokio::runtime::Handle,
    db: Arc<DbConnection>,
    docker: Arc<bollard::Docker>,
    snapshot_dir: std::path::PathBuf,
    overlay_dns_slot: temps_dns::OverlayDnsSlot,
) {
    runtime.spawn(async move {
        let docker_runtime = temps_deployer::docker::DockerRuntime::new(
            docker,
            true,
            temps_core::NETWORK_NAME.to_string(),
        );
        let mut active: Option<temps_dns::ControlPlaneResolver> = None;
        let mut failures = 0u32;
        loop {
            // Only cancellable Docker operations are timed out. Once DNS has
            // bound port 53, its handle must be explicitly shut down.
            let gateway = tokio::time::timeout(Duration::from_secs(10), async {
                docker_runtime.ensure_network_exists().await?;
                docker_runtime
                    .inspect_app_network_gateway()
                    .await
                    .ok_or(ProxyDnsBootstrapError::MissingGateway)
            })
            .await
            .unwrap_or(Err(ProxyDnsBootstrapError::NetworkTimeout));

            let attempt = match gateway {
                Ok(gateway) => {
                    let healthy = if active.as_ref().is_some_and(|resolver| resolver.gateway() == gateway) {
                        temps_dns::probe_control_plane_resolver(std::net::SocketAddr::new(gateway, 53)).await
                    } else {
                        false
                    };
                    if healthy {
                        publish_dns_gateway(&overlay_dns_slot, Some(gateway))
                    } else {
                        let cleared = publish_dns_gateway(&overlay_dns_slot, None);
                        if let Some(resolver) = active.take() {
                            resolver.shutdown().await;
                        }
                        match cleared {
                            Err(error) => Err(error),
                            Ok(()) => match temps_dns::start_control_plane_resolver(
                                db.clone(), gateway, snapshot_dir.clone(),
                            ).await {
                                Some(resolver) => {
                                    active = Some(resolver);
                                    publish_dns_gateway(&overlay_dns_slot, Some(gateway))
                                }
                                None => Err(ProxyDnsBootstrapError::Resolver(gateway)),
                            }
                        }
                    }
                }
                Err(error) => Err(error),
            };
            match attempt {
                Ok(()) => failures = 0,
                Err(error) => {
                    let _ = publish_dns_gateway(&overlay_dns_slot, None);
                    if let Some(resolver) = active.take() {
                        resolver.shutdown().await;
                    }
                    failures += 1;
                    if failures >= DNS_MAX_CONSECUTIVE_FAILURES {
                        warn!(error = %error, attempts = failures, "Proxy cluster DNS reconciliation stopped after repeated failures");
                        break;
                    }
                    warn!(error = %error, attempts = failures, "Proxy cluster DNS reconciliation failed; retrying");
                }
            }
            tokio::time::sleep(DNS_RETRY_INTERVAL).await;
        }
    });
}

pub(crate) fn spawn_control_plane_dns_bootstrap_with_docker_discovery(
    runtime: &tokio::runtime::Handle,
    db: Arc<DbConnection>,
    snapshot_dir: std::path::PathBuf,
    overlay_dns_slot: temps_dns::OverlayDnsSlot,
) {
    let runtime_handle = runtime.clone();
    runtime.spawn(async move {
        for attempt in 1..=DNS_MAX_CONSECUTIVE_FAILURES {
            let docker = tokio::time::timeout(Duration::from_secs(5), async {
                let docker = bollard::Docker::connect_with_defaults()?;
                docker.ping().await?;
                Ok::<_, ProxyDnsBootstrapError>(Arc::new(docker))
            })
            .await;
            match docker {
                Ok(Ok(docker)) => {
                    spawn_control_plane_dns_bootstrap(
                        &runtime_handle,
                        db,
                        docker,
                        snapshot_dir,
                        overlay_dns_slot,
                    );
                    break;
                }
                Ok(Err(error)) => warn!(error = %error, attempt, "Proxy cluster DNS Docker discovery failed; retrying"),
                Err(_) => warn!(attempt, "Proxy cluster DNS Docker discovery timed out; retrying"),
            }
            if attempt < DNS_MAX_CONSECUTIVE_FAILURES {
                tokio::time::sleep(DNS_RETRY_INTERVAL).await;
            }
        }
        warn!(attempts = DNS_MAX_CONSECUTIVE_FAILURES, "Proxy cluster DNS Docker discovery stopped after repeated failures");
    });
}

/// Keep HTTP serving available when optional Docker-backed features cannot be
/// initialized. Both combined and split proxy composition roots use this gate,
/// so an unavailable socket cannot accidentally become a startup failure in
/// one mode only.
pub(crate) fn optional_docker_feature<T>(
    result: anyhow::Result<T>,
    disabled_features: &str,
) -> Option<T> {
    match result {
        Ok(value) => Some(value),
        Err(error) => {
            warn!(
                "Docker not available — {} will be disabled: {}",
                disabled_features, error
            );
            None
        }
    }
}

/// Adapter bridging `temps_deployer::ContainerDeployer` to `temps_proxy::on_demand::ContainerLifecycle`.
pub(crate) struct ContainerLifecycleAdapter {
    deployer: Arc<dyn ContainerDeployer>,
    runtime_context: Arc<temps_core::RuntimeContext>,
}

impl ContainerLifecycleAdapter {
    pub fn new(
        deployer: Arc<dyn ContainerDeployer>,
        runtime_context: Arc<temps_core::RuntimeContext>,
    ) -> Self {
        Self {
            deployer,
            runtime_context,
        }
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
        let check = check_accepting_requests(
            &self.deployer,
            container_id,
            Duration::from_secs(2),
            self.runtime_context.as_ref(),
        )
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
    project_ip_gate: Arc<dyn temps_core::ProjectIpGate>,
    request_policy_gate: Arc<dyn temps_core::RequestPolicyGate>,
    overlay_dns_slot: temps_dns::OverlayDnsSlot,
    docker: Option<Arc<bollard::Docker>>,
    local_workloads_enabled: bool,
) -> anyhow::Result<()> {
    let console_address = config.console_address.clone();
    // Runtime for the startup settings fetch AND, when ADR-018 on-demand TLS is
    // enabled, the long-lived home of `OnDemandCertManager`'s issuance consumer
    // (`OnDemandCertManager::new` calls `tokio::spawn` on whatever runtime is
    // current). It therefore has to outlive this fetch and it has to stay
    // multi-threaded: on a `current_thread` runtime, a thread parked by Pingora
    // would leave the consumer unpolled and silently break issuance.
    //
    // `Runtime::new()` sizes itself to the host (one worker per core), which on
    // a big box means ~24 idle worker threads costing ~100 kB of anonymous RSS
    // each for a runtime whose entire steady-state workload is one consumer
    // task draining a bounded channel. Two workers keep the "a blocked thread
    // can't stall the consumer" property at a fixed cost.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("temps-proxy-ctl")
        .enable_all()
        .build()?;

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
    let internal_dns_sync_address = match settings.as_ref() {
        Some(settings) if settings.cluster_dns.enabled => {
            match rt.block_on(temps_dns::start_proxy_dns_sync_service(db.clone())) {
                Ok(address) => Some(address.to_string()),
                Err(error) => {
                    warn!(error = %error, "Proxy DNS sync service is unavailable; HTTP proxy startup will continue");
                    None
                }
            }
        }
        _ => None,
    };
    if settings
        .as_ref()
        .is_some_and(|settings| settings.cluster_dns.enabled)
    {
        if let Some(docker) = docker {
            spawn_control_plane_dns_bootstrap(
                rt.handle(),
                db.clone(),
                docker,
                config.data_dir.join("dns"),
                overlay_dns_slot.clone(),
            );
        } else if local_workloads_enabled {
            spawn_control_plane_dns_bootstrap_with_docker_discovery(
                rt.handle(),
                db.clone(),
                config.data_dir.join("dns"),
                overlay_dns_slot.clone(),
            );
        }
    }

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
        internal_dns_sync_address,
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
    let stateless_instance_id = rt.block_on(temps_config::stateless_instance_id(db.as_ref()))?;
    let stateless_storage = temps_file_store::s3_config::resolve_stateless_storage_for(
        stateless_instance_id.as_deref(),
    )
    .map_err(|error| anyhow::anyhow!("❌ Stateless storage configuration is invalid\n\n{error}"))?;

    match temps_proxy::setup_proxy_server(
        db,
        proxy_config,
        cookie_crypto,
        encryption_service,
        route_table,
        shutdown_signal,
        config.clone(),
        stateless_storage,
        on_demand_manager,
        admin_gate,
        retention_resolver,
        project_ip_gate,
        request_policy_gate,
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

    #[test]
    fn unavailable_docker_disables_optional_features_without_failing_startup() {
        let result = optional_docker_feature::<()>(
            Err(anyhow::anyhow!("Docker socket is unavailable")),
            "on-demand test features",
        );

        assert!(result.is_none());
    }
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

    /// Build a `ContainerInfo` pointed at a test listener. The injected Host
    /// runtime context resolves it to `("127.0.0.1", host_port)`. Using
    /// `container_name = "127.0.0.1"` and
    /// `container_port == host_port` makes both modes resolve to
    /// `http://127.0.0.1:{port}/`.
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
                    host_ip: None,
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
        ContainerLifecycleAdapter::new(
            Arc::new(MockDeployer { info }),
            Arc::new(temps_core::RuntimeContext::host()),
        )
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
