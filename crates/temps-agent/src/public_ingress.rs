// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Public worker ingress. Configuration and routes come exclusively from the
//! authenticated control-plane snapshot. Unknown SNI and Host values fail
//! closed; there is no control-plane console fallback.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use serde::Serialize;
use thiserror::Error;
use tokio::sync::Notify;
use tracing::{info, warn};

use crate::route_store::SharedRouteStore;

#[derive(Clone)]
pub struct PublicIngressConfig {
    pub http_address: SocketAddr,
    pub https_address: SocketAddr,
    /// Base64 X25519 private key generated during worker enrollment.
    pub private_key_b64: String,
    pub control_plane_url: String,
    pub node_id: i32,
    pub node_token: String,
}

#[derive(Debug, Error)]
pub enum PublicIngressError {
    #[error("failed to bind public ingress listener {address}: {reason}")]
    Bind { address: SocketAddr, reason: String },
    #[error("failed to configure public ingress TLS for {address}: {reason}")]
    Tls { address: SocketAddr, reason: String },
}

pub struct PublicIngressHandle {
    http_address: SocketAddr,
    https_address: SocketAddr,
    running: Arc<AtomicBool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PublicIngressHealth {
    pub running: bool,
    pub last_error: Option<String>,
    pub certificate_count: i64,
    pub route_count: i64,
    pub unsupported_route_count: i64,
    pub unsupported_reasons: Vec<String>,
}

static HEALTH: std::sync::OnceLock<RwLock<PublicIngressHealth>> = std::sync::OnceLock::new();
static LISTENER_RUNNING: AtomicBool = AtomicBool::new(false);

pub fn health() -> Option<PublicIngressHealth> {
    HEALTH
        .get()
        .and_then(|health| health.read().ok().map(|value| value.clone()))
}

pub fn record_failure(error: impl Into<String>) {
    let health = HEALTH.get_or_init(|| {
        RwLock::new(PublicIngressHealth {
            running: false,
            last_error: None,
            certificate_count: 0,
            route_count: 0,
            unsupported_route_count: 0,
            unsupported_reasons: Vec::new(),
        })
    });
    if let Ok(mut health) = health.write() {
        LISTENER_RUNNING.store(false, Ordering::Release);
        health.running = false;
        health.last_error = Some(error.into());
    }
}

pub fn update_snapshot_health(store: &SharedRouteStore) {
    let snapshot = store.public_ingress_snapshot();
    let (configured_enabled, authorized, prepared_certificate_count) =
        store.public_ingress_runtime_status();
    let missing_certificates = snapshot
        .routes
        .len()
        .saturating_sub(prepared_certificate_count);
    let health = HEALTH.get_or_init(|| {
        RwLock::new(PublicIngressHealth {
            running: false,
            last_error: None,
            certificate_count: 0,
            route_count: 0,
            unsupported_route_count: 0,
            unsupported_reasons: Vec::new(),
        })
    });
    if let Ok(mut health) = health.write() {
        let listener_running = LISTENER_RUNNING.load(Ordering::Acquire);
        health.running = listener_running && configured_enabled && authorized;
        health.certificate_count = prepared_certificate_count as i64;
        health.route_count = snapshot.routes.len() as i64;
        health.unsupported_route_count = snapshot.unsupported_route_count as i64;
        health
            .unsupported_reasons
            .clone_from(&snapshot.unsupported_reasons);
        if !listener_running {
            if health.last_error.is_none() {
                health.last_error = Some("public ingress listener is not running".to_string());
            }
        } else if configured_enabled && !authorized {
            health.last_error = Some(
                "public ingress authorization lease expired; waiting for control-plane sync"
                    .to_string(),
            );
        } else if missing_certificates > 0 {
            health.last_error = Some(format!(
                "{missing_certificates} public route(s) have no prepared exact TLS certificate; HTTPS is unavailable for those hosts"
            ));
            health.unsupported_reasons.push(
                "wildcard certificate keys are not distributed to workers; affected hosts require exact leaf certificates"
                    .to_string(),
            );
        } else {
            health.last_error = None;
        }
    }
}

impl PublicIngressHandle {
    pub fn http_address(&self) -> SocketAddr {
        self.http_address
    }
    pub fn https_address(&self) -> SocketAddr {
        self.https_address
    }
    pub fn is_ready(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }
    pub fn last_error(&self) -> Option<String> {
        health().and_then(|health| health.last_error)
    }
}

/// Bind both listeners. Port zero is supported and the actual bound addresses
/// are returned for integration tests and custom bootstrap configurations.
pub async fn spawn(
    config: PublicIngressConfig,
    store: SharedRouteStore,
    shutdown: Arc<Notify>,
) -> Result<PublicIngressHandle, PublicIngressError> {
    let http = tokio::net::TcpListener::bind(config.http_address)
        .await
        .map_err(|error| PublicIngressError::Bind {
            address: config.http_address,
            reason: error.to_string(),
        })?;
    let https = tokio::net::TcpListener::bind(config.https_address)
        .await
        .map_err(|error| PublicIngressError::Bind {
            address: config.https_address,
            reason: error.to_string(),
        })?;
    let http_address = http
        .local_addr()
        .map_err(|error| PublicIngressError::Bind {
            address: config.http_address,
            reason: error.to_string(),
        })?;
    let https_address = https
        .local_addr()
        .map_err(|error| PublicIngressError::Bind {
            address: config.https_address,
            reason: error.to_string(),
        })?;

    let http_router = crate::internal_proxy::public_router(
        Arc::clone(&store),
        "http",
        Some(crate::internal_proxy::PublicAcmeConfig {
            control_plane_url: config.control_plane_url.clone(),
            node_id: config.node_id,
            node_token: config.node_token.clone(),
        }),
    )
    .map_err(|error| PublicIngressError::Bind {
        address: http_address,
        reason: error.to_string(),
    })?;
    let https_router = crate::internal_proxy::public_router(Arc::clone(&store), "https", None)
        .map_err(|error| PublicIngressError::Tls {
            address: https_address,
            reason: error.to_string(),
        })?;
    store.configure_public_tls(config.private_key_b64);
    let running = Arc::new(AtomicBool::new(true));
    LISTENER_RUNNING.store(true, Ordering::Release);
    let http_shutdown = Arc::clone(&shutdown);
    tokio::spawn(serve_http(
        http,
        http_router,
        http_shutdown,
        Arc::clone(&running),
    ));

    let resolver = Arc::new(SnapshotCertResolver::new(Arc::clone(&store)));
    let tls = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_cert_resolver(resolver);
    let tls_shutdown = Arc::clone(&shutdown);
    tokio::spawn(serve_tls(
        https,
        https_router,
        Arc::new(tls),
        tls_shutdown,
        Arc::clone(&running),
    ));
    info!(%http_address, %https_address, "public worker ingress listening");
    let snapshot = store.public_ingress_snapshot();
    let (_, _, prepared_certificate_count) = store.public_ingress_runtime_status();
    let health = HEALTH.get_or_init(|| {
        RwLock::new(PublicIngressHealth {
            running: true,
            last_error: None,
            certificate_count: prepared_certificate_count as i64,
            route_count: snapshot.routes.len() as i64,
            unsupported_route_count: snapshot.unsupported_route_count as i64,
            unsupported_reasons: snapshot.unsupported_reasons.clone(),
        })
    });
    if let Ok(mut health) = health.write() {
        *health = PublicIngressHealth {
            running: true,
            last_error: None,
            certificate_count: prepared_certificate_count as i64,
            route_count: snapshot.routes.len() as i64,
            unsupported_route_count: snapshot.unsupported_route_count as i64,
            unsupported_reasons: snapshot.unsupported_reasons.clone(),
        };
    }
    update_snapshot_health(&store);
    let health_store = Arc::clone(&store);
    let health_shutdown = Arc::clone(&shutdown);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
        loop {
            tokio::select! {
                _ = interval.tick() => update_snapshot_health(&health_store),
                _ = health_shutdown.notified() => break,
            }
        }
    });
    Ok(PublicIngressHandle {
        http_address,
        https_address,
        running,
    })
}

struct SnapshotCertResolver {
    store: SharedRouteStore,
}

impl std::fmt::Debug for SnapshotCertResolver {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SnapshotCertResolver")
            .finish_non_exhaustive()
    }
}

impl SnapshotCertResolver {
    fn new(store: SharedRouteStore) -> Self {
        Self { store }
    }

    fn load(&self, host: &str) -> Option<Arc<CertifiedKey>> {
        self.store.lookup_public(host)?;
        self.store.lookup_public_tls_key(host)
    }
}

impl ResolvesServerCert for SnapshotCertResolver {
    fn resolve(&self, client_hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        self.load(
            client_hello
                .server_name()?
                .trim_end_matches('.')
                .to_ascii_lowercase()
                .as_str(),
        )
    }
}

async fn serve_tls(
    listener: tokio::net::TcpListener,
    router: axum::Router,
    config: Arc<rustls::ServerConfig>,
    shutdown: Arc<Notify>,
    running: Arc<AtomicBool>,
) {
    use hyper_util::rt::{TokioExecutor, TokioIo};
    use tower::Service;
    let acceptor = tokio_rustls::TlsAcceptor::from(config);
    let connections = Arc::new(tokio::sync::Semaphore::new(1024));
    loop {
        let accepted = tokio::select! { _ = shutdown.notified() => break, accepted = listener.accept() => accepted };
        let Ok((stream, peer)) = accepted else {
            record_failure("public HTTPS listener failed while accepting a connection");
            break;
        };
        let acceptor = acceptor.clone();
        let router = router.clone();
        let Ok(connection_permit) = Arc::clone(&connections).try_acquire_owned() else {
            continue;
        };
        tokio::spawn(async move {
            let connection_permit = Arc::new(connection_permit);
            let Ok(Ok(stream)) =
                tokio::time::timeout(std::time::Duration::from_secs(10), acceptor.accept(stream))
                    .await
            else {
                return;
            };
            let Some(sni) = stream
                .get_ref()
                .1
                .server_name()
                .map(|name| name.trim_end_matches('.').to_ascii_lowercase())
            else {
                return;
            };
            let first_request = Arc::new(Notify::new());
            let request_signal = Arc::clone(&first_request);
            let service = hyper::service::service_fn(move |mut request| {
                request_signal.notify_one();
                request
                    .extensions_mut()
                    .insert(crate::internal_proxy::PublicTlsSni(sni.clone()));
                request
                    .extensions_mut()
                    .insert(crate::internal_proxy::PublicPeer(peer));
                request
                    .extensions_mut()
                    .insert(crate::internal_proxy::PublicConnectionPermit(Arc::clone(
                        &connection_permit,
                    )));
                let mut router = router.clone();
                async move { router.call(request).await }
            });
            let mut builder = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new());
            builder
                .http1()
                .timer(hyper_util::rt::TokioTimer::new())
                .header_read_timeout(std::time::Duration::from_secs(10))
                .keep_alive(false);
            let connection = builder.serve_connection_with_upgrades(TokioIo::new(stream), service);
            tokio::pin!(connection);
            tokio::select! {
                result = &mut connection => if let Err(error) = result {
                    warn!(%error, "public HTTPS connection failed");
                },
                _ = first_request.notified() => if let Err(error) = connection.await {
                    warn!(%error, "public HTTPS connection failed");
                },
                _ = tokio::time::sleep(std::time::Duration::from_secs(10)) => {}
            }
        });
    }
    running.store(false, Ordering::Release);
    LISTENER_RUNNING.store(false, Ordering::Release);
}

async fn serve_http(
    listener: tokio::net::TcpListener,
    router: axum::Router,
    shutdown: Arc<Notify>,
    running: Arc<AtomicBool>,
) {
    use hyper_util::rt::{TokioExecutor, TokioIo};
    use tower::Service;
    let connections = Arc::new(tokio::sync::Semaphore::new(1024));
    loop {
        let accepted = tokio::select! { _ = shutdown.notified() => break, accepted = listener.accept() => accepted };
        let Ok((stream, peer)) = accepted else {
            record_failure("public HTTP listener failed while accepting a connection");
            break;
        };
        let Ok(connection_permit) = Arc::clone(&connections).try_acquire_owned() else {
            continue;
        };
        let router = router.clone();
        tokio::spawn(async move {
            let connection_permit = Arc::new(connection_permit);
            let first_request = Arc::new(Notify::new());
            let request_signal = Arc::clone(&first_request);
            let service = hyper::service::service_fn(move |mut request| {
                request_signal.notify_one();
                request
                    .extensions_mut()
                    .insert(crate::internal_proxy::PublicPeer(peer));
                request
                    .extensions_mut()
                    .insert(crate::internal_proxy::PublicConnectionPermit(Arc::clone(
                        &connection_permit,
                    )));
                let mut router = router.clone();
                async move { router.call(request).await }
            });
            let mut builder = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new());
            builder
                .http1()
                .timer(hyper_util::rt::TokioTimer::new())
                .header_read_timeout(std::time::Duration::from_secs(10))
                .keep_alive(false);
            let connection = builder.serve_connection_with_upgrades(TokioIo::new(stream), service);
            tokio::pin!(connection);
            tokio::select! {
                result = &mut connection => if let Err(error) = result {
                    warn!(%error, "public HTTP connection failed");
                },
                _ = first_request.notified() => if let Err(error) = connection.await {
                    warn!(%error, "public HTTP connection failed");
                },
                _ = tokio::time::sleep(std::time::Duration::from_secs(10)) => {}
            }
        });
    }
    running.store(false, Ordering::Release);
    LISTENER_RUNNING.store(false, Ordering::Release);
}
