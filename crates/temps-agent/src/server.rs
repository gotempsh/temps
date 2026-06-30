//! Agent HTTP server setup and routing.

use axum::{
    middleware,
    routing::{delete, get, post},
    Extension, Router,
};
use std::sync::Arc;
use std::time::Duration;
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;

use crate::auth::{require_agent_auth, AgentAuth};
use crate::handlers::{self, AgentApiDoc, AgentState};
use crate::service_handlers;
use crate::AgentConfig;
use temps_deployer::{ContainerDeployer, ImageBuilder};

/// Build the agent Axum router with authentication middleware.
pub fn build_router(
    container_deployer: Arc<dyn ContainerDeployer>,
    image_builder: Arc<dyn ImageBuilder>,
    docker: Option<bollard::Docker>,
    config: &AgentConfig,
    overlay_bridge_address: Arc<std::sync::RwLock<Option<std::net::IpAddr>>>,
    overlay_peers: crate::network_sync::SharedPeers,
) -> Router {
    let state = Arc::new(AgentState {
        container_deployer,
        image_builder,
        docker,
        overlay_bridge_address,
        overlay_peers,
    });

    let auth = Arc::new(AgentAuth::new(&config.token));

    // API routes — all protected by bearer token auth
    let api_routes = Router::new()
        // Container management routes
        .route("/agent/containers/deploy", post(handlers::deploy_container))
        .route(
            "/agent/containers/{id}/stop",
            post(handlers::stop_container),
        )
        .route(
            "/agent/containers/{id}/start",
            post(handlers::start_container),
        )
        .route(
            "/agent/containers/{id}/exec",
            post(handlers::exec_container),
        )
        .route(
            "/agent/containers/{id}/terminal",
            get(handlers::terminal_container),
        )
        .route("/agent/containers/{id}", delete(handlers::remove_container))
        .route(
            "/agent/containers/{id}/logs",
            get(handlers::get_container_logs),
        )
        .route(
            "/agent/containers/{id}/logs/stream",
            get(handlers::stream_container_logs),
        )
        .route(
            "/agent/containers/{id}/stats",
            get(handlers::get_container_stats),
        )
        .route(
            "/agent/containers/{id}/info",
            get(handlers::get_container_info),
        )
        .route("/agent/containers", get(handlers::list_containers))
        .route("/agent/images/import", post(handlers::import_image))
        .route("/agent/images/{name}/exists", get(handlers::image_exists))
        .route("/agent/health", get(handlers::health_check))
        // Service management routes
        .route("/agent/services", post(service_handlers::create_service))
        .route("/agent/services", get(service_handlers::list_services))
        .route(
            "/agent/services/{name}/stop",
            post(service_handlers::stop_service),
        )
        .route(
            "/agent/services/{name}/start",
            post(service_handlers::start_service),
        )
        .route(
            "/agent/services/{name}",
            delete(service_handlers::remove_service),
        )
        .route(
            "/agent/services/{name}/status",
            get(service_handlers::service_status),
        )
        .route("/agent/services/exec", post(service_handlers::service_exec))
        .route(
            "/agent/services/backup",
            post(service_handlers::backup_service),
        )
        .route(
            "/agent/services/restore",
            post(service_handlers::restore_service),
        )
        .layer(middleware::from_fn(require_agent_auth))
        .layer(Extension(auth))
        .with_state(state);

    // Swagger UI — no auth required so it's accessible for documentation
    let swagger_ui =
        SwaggerUi::new("/swagger-ui").url("/api-docs/openapi.json", AgentApiDoc::openapi());

    api_routes.merge(swagger_ui)
}

/// Maximum number of consecutive heartbeat failures before escalating to error-level logging.
const HEARTBEAT_MAX_RETRIES: u32 = 3;

/// Base delay for exponential backoff between heartbeat retries (doubled each attempt).
const HEARTBEAT_RETRY_BASE_DELAY: Duration = Duration::from_secs(2);

/// Maximum backoff delay between heartbeat retries.
const HEARTBEAT_RETRY_MAX_DELAY: Duration = Duration::from_secs(15);

/// Spawn a background task that sends heartbeats to the control plane every 30 seconds.
///
/// On transient failures, retries up to `HEARTBEAT_MAX_RETRIES` times with exponential
/// backoff before giving up for this interval. This prevents a brief network blip from
/// causing the control plane to mark the node as offline (90s stale threshold).
///
/// The first successful heartbeat includes a full container inventory so the control
/// plane can reconcile stale DB records against actual Docker state (e.g., after a crash).
fn spawn_heartbeat_loop(
    config: &AgentConfig,
    container_deployer: Arc<dyn temps_deployer::ContainerDeployer>,
) {
    let control_plane_url = config.control_plane_url.clone();
    let node_id = config.node_id;
    let token = config.token.clone();
    let labels = config.labels.clone();

    tokio::spawn(async move {
        // Strict TLS — the worker→control-plane heartbeat carries the
        // node's auth token. A MitM with a self-signed cert here would
        // capture the token and impersonate this worker. There is no
        // opt-in: `AppSettings.insecure_tls` is server-side only.
        let client = match reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                tracing::error!("Failed to build heartbeat HTTP client: {}", e);
                return;
            }
        };

        let heartbeat_url = format!(
            "{}/api/internal/nodes/{}/heartbeat",
            control_plane_url, node_id
        );

        let mut interval = tokio::time::interval(Duration::from_secs(30));
        let mut consecutive_failures: u32 = 0;
        let mut inventory_sent = false;

        loop {
            interval.tick().await;

            let capacity = collect_capacity_metrics();
            let mut body = serde_json::json!({ "capacity": capacity, "labels": labels });

            // On the first heartbeat (agent startup/reconnect), include a full
            // container inventory so the control plane can reconcile stale state.
            if !inventory_sent {
                match container_deployer.list_containers().await {
                    Ok(containers) => {
                        // Only include temps-managed containers
                        let managed: Vec<_> = containers
                            .into_iter()
                            .filter(|c| {
                                c.labels
                                    .get("sh.temps.managed")
                                    .map(|v| v == "true")
                                    .unwrap_or(false)
                            })
                            .map(|c| {
                                serde_json::json!({
                                    "container_id": c.container_id,
                                    "container_name": c.container_name,
                                })
                            })
                            .collect();
                        body["containers"] = serde_json::json!(managed);
                        tracing::info!(
                            node_id = node_id,
                            count = managed.len(),
                            "Including container inventory in heartbeat for reconciliation"
                        );
                    }
                    Err(e) => {
                        tracing::warn!(
                            node_id = node_id,
                            "Failed to list containers for inventory: {}",
                            e
                        );
                    }
                }
            }

            let mut attempt = 0;
            let mut succeeded = false;

            loop {
                match client
                    .post(&heartbeat_url)
                    .bearer_auth(&token)
                    .json(&body)
                    .send()
                    .await
                {
                    Ok(response) if response.status().is_success() => {
                        if consecutive_failures > 0 {
                            tracing::info!(
                                node_id = node_id,
                                previous_failures = consecutive_failures,
                                "Heartbeat recovered after {} consecutive failure(s)",
                                consecutive_failures
                            );
                        }
                        consecutive_failures = 0;
                        succeeded = true;
                        inventory_sent = true;
                        tracing::debug!(node_id = node_id, "Heartbeat sent to control plane");
                        break;
                    }
                    Ok(response)
                        if response.status().is_server_error()
                            && attempt < HEARTBEAT_MAX_RETRIES =>
                    {
                        // Server errors are retryable
                        attempt += 1;
                        let delay = std::cmp::min(
                            HEARTBEAT_RETRY_BASE_DELAY * 2u32.saturating_pow(attempt - 1),
                            HEARTBEAT_RETRY_MAX_DELAY,
                        );
                        tracing::warn!(
                            node_id = node_id,
                            attempt = attempt,
                            status = %response.status(),
                            retry_in_ms = delay.as_millis() as u64,
                            "Heartbeat failed with server error, retrying"
                        );
                        tokio::time::sleep(delay).await;
                    }
                    Ok(response) => {
                        // Client error (4xx) or exhausted retries — don't retry
                        tracing::warn!(
                            node_id = node_id,
                            status = %response.status(),
                            "Heartbeat failed with status {}",
                            response.status()
                        );
                        break;
                    }
                    Err(e) if attempt < HEARTBEAT_MAX_RETRIES => {
                        // Network errors are retryable
                        attempt += 1;
                        let delay = std::cmp::min(
                            HEARTBEAT_RETRY_BASE_DELAY * 2u32.saturating_pow(attempt - 1),
                            HEARTBEAT_RETRY_MAX_DELAY,
                        );
                        tracing::warn!(
                            node_id = node_id,
                            attempt = attempt,
                            retry_in_ms = delay.as_millis() as u64,
                            "Heartbeat network error, retrying: {}",
                            e
                        );
                        tokio::time::sleep(delay).await;
                    }
                    Err(e) => {
                        tracing::warn!(
                            node_id = node_id,
                            attempts = attempt + 1,
                            "Heartbeat failed after {} attempt(s): {}",
                            attempt + 1,
                            e
                        );
                        break;
                    }
                }
            }

            if !succeeded {
                consecutive_failures += 1;
                if consecutive_failures >= 3 {
                    tracing::error!(
                        node_id = node_id,
                        consecutive_failures = consecutive_failures,
                        "Heartbeat has failed {} consecutive times — node may be marked offline",
                        consecutive_failures
                    );
                }
            }
        }
    });
}

/// Collect system resource metrics for heartbeat capacity data.
fn collect_capacity_metrics() -> serde_json::Value {
    use sysinfo::{CpuExt, DiskExt, SystemExt};

    let mut sys = sysinfo::System::new();
    sys.refresh_cpu();
    sys.refresh_memory();
    sys.refresh_disks_list();
    sys.refresh_disks();

    // Use only the root mount point to avoid double-counting overlapping mounts
    let (disk_used, disk_total) = sys
        .disks()
        .iter()
        .find(|d| d.mount_point() == std::path::Path::new("/"))
        .map(|d| (d.total_space() - d.available_space(), d.total_space()))
        .unwrap_or((0, 0));

    serde_json::json!({
        "cpu_percent": sys.global_cpu_info().cpu_usage(),
        "memory_used_bytes": sys.used_memory(),
        "memory_total_bytes": sys.total_memory(),
        "disk_used_bytes": disk_used,
        "disk_total_bytes": disk_total,
    })
}

/// Start the agent server. This blocks until the server shuts down.
pub async fn start_agent_server(
    container_deployer: Arc<dyn ContainerDeployer>,
    image_builder: Arc<dyn ImageBuilder>,
    docker: Option<bollard::Docker>,
    config: AgentConfig,
    overlay_peers: crate::network_sync::SharedPeers,
    overlay_bridge_address: Arc<std::sync::RwLock<Option<std::net::IpAddr>>>,
) -> Result<(), crate::AgentError> {
    let router = build_router(
        container_deployer.clone(),
        image_builder,
        docker,
        &config,
        overlay_bridge_address.clone(),
        overlay_peers.clone(),
    );

    // Start heartbeat background loop (with deployer for container inventory on first beat)
    spawn_heartbeat_loop(&config, container_deployer);

    // Start the multi-host network sync loop. Failures here NEVER stop the
    // agent — when this node has no compute_cidr allocated (single-host
    // cluster, or simply not yet allocated), the loop is a no-op. When a
    // compute_cidr is allocated, the loop bootstraps the overlay and keeps
    // peers reconciled. `temps join` semantics are unchanged either way.
    crate::network_sync::spawn(&config, overlay_bridge_address.clone(), overlay_peers);

    let listener = tokio::net::TcpListener::bind(&config.listen_address)
        .await
        .map_err(|e| {
            crate::AgentError::ServerError(format!(
                "Failed to bind to {}: {}",
                config.listen_address, e
            ))
        })?;

    tracing::info!(
        address = %config.listen_address,
        node = %config.node_name,
        node_id = config.node_id,
        swagger_ui = format!("http://{}/swagger-ui/", config.listen_address),
        "Temps agent server started"
    );

    // Serve mutual TLS when the node has been provisioned with certs
    // (ADR-020 WS-2.1); otherwise plain HTTP for legacy / not-yet-enrolled
    // nodes. The mTLS path verifies the control plane's client certificate
    // against the cluster CA on every connection.
    match (
        config.tls_cert_path.as_ref(),
        config.tls_key_path.as_ref(),
        config.cluster_ca_path.as_ref(),
    ) {
        (Some(cert), Some(key), Some(ca)) => {
            tracing::info!(
                cert = %cert.display(),
                "Agent serving with mutual TLS (control-plane client cert verified against cluster CA)"
            );
            let server_config = build_tls_server_config(cert, key, ca)?;
            serve_mtls(listener, router, std::sync::Arc::new(server_config)).await?;
        }
        _ => {
            axum::serve(listener, router).await.map_err(|e| {
                crate::AgentError::ServerError(format!("Agent server error: {}", e))
            })?;
        }
    }

    Ok(())
}

/// Build a rustls `ServerConfig` that serves the node's leaf cert and requires
/// the client (the control plane) to present a certificate chaining to the
/// cluster CA (ADR-020 WS-2.1).
fn build_tls_server_config(
    cert_path: &std::path::Path,
    key_path: &std::path::Path,
    ca_path: &std::path::Path,
) -> Result<rustls::ServerConfig, crate::AgentError> {
    use rustls::pki_types::{CertificateDer, PrivateKeyDer};
    use std::io::BufReader;

    let tls_err = |context: &str, reason: String| crate::AgentError::TlsConfig {
        context: context.to_string(),
        reason,
    };

    let cert_bytes =
        std::fs::read(cert_path).map_err(|e| tls_err("read leaf cert", e.to_string()))?;
    let cert_chain: Vec<CertificateDer<'static>> =
        rustls_pemfile::certs(&mut BufReader::new(&cert_bytes[..]))
            .collect::<Result<_, _>>()
            .map_err(|e| tls_err("parse leaf cert", e.to_string()))?;
    if cert_chain.is_empty() {
        return Err(tls_err("parse leaf cert", "no certificates found".into()));
    }

    let key_bytes = std::fs::read(key_path).map_err(|e| tls_err("read node key", e.to_string()))?;
    let key: PrivateKeyDer<'static> =
        rustls_pemfile::private_key(&mut BufReader::new(&key_bytes[..]))
            .map_err(|e| tls_err("parse node key", e.to_string()))?
            .ok_or_else(|| tls_err("parse node key", "no private key found".into()))?;

    let ca_bytes = std::fs::read(ca_path).map_err(|e| tls_err("read cluster CA", e.to_string()))?;
    let ca_certs: Vec<CertificateDer<'static>> =
        rustls_pemfile::certs(&mut BufReader::new(&ca_bytes[..]))
            .collect::<Result<_, _>>()
            .map_err(|e| tls_err("parse cluster CA", e.to_string()))?;
    let mut roots = rustls::RootCertStore::empty();
    for c in ca_certs {
        roots
            .add(c)
            .map_err(|e| tls_err("add cluster CA root", e.to_string()))?;
    }
    // A non-empty root store is required; an empty/garbage CA fails here rather
    // than silently allowing any client.
    let verifier = rustls::server::WebPkiClientVerifier::builder(std::sync::Arc::new(roots))
        .build()
        .map_err(|e| tls_err("build client-cert verifier", e.to_string()))?;

    rustls::ServerConfig::builder()
        .with_client_cert_verifier(verifier)
        .with_single_cert(cert_chain, key)
        .map_err(|e| tls_err("build server config", e.to_string()))
}

/// Hand-rolled accept loop that TLS-terminates each connection and drives the
/// axum router over hyper with WebSocket-upgrade support (the agent exposes a
/// terminal WS route, so `serve_connection_with_upgrades` is required). Mirrors
/// the pattern in `temps-plugin-sdk/src/runtime.rs`.
async fn serve_mtls(
    listener: tokio::net::TcpListener,
    router: Router,
    server_config: std::sync::Arc<rustls::ServerConfig>,
) -> Result<(), crate::AgentError> {
    use hyper_util::rt::{TokioExecutor, TokioIo};
    use tower::Service;

    let acceptor = tokio_rustls::TlsAcceptor::from(server_config);

    loop {
        let (stream, _peer) = match listener.accept().await {
            Ok(pair) => pair,
            Err(e) => {
                tracing::error!("agent: failed to accept connection: {}", e);
                continue;
            }
        };
        let acceptor = acceptor.clone();
        let router = router.clone();
        tokio::spawn(async move {
            let tls_stream = match acceptor.accept(stream).await {
                Ok(s) => s,
                Err(e) => {
                    // Rejected client (missing/invalid cert) or handshake error.
                    tracing::warn!("agent: TLS handshake rejected: {}", e);
                    return;
                }
            };
            let socket = TokioIo::new(tls_stream);
            let hyper_service =
                hyper::service::service_fn(move |req: hyper::Request<hyper::body::Incoming>| {
                    let mut router = router.clone();
                    async move { router.call(req).await }
                });
            if let Err(err) = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new())
                .serve_connection_with_upgrades(socket, hyper_service)
                .await
            {
                let msg = err.to_string();
                if !msg.contains("shutting down") {
                    tracing::warn!("agent: connection error: {}", msg);
                }
            }
        });
    }
}
