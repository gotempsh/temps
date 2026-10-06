// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The node API on the control plane's mesh address (ADR 048 D3).
//!
//! A node paired from the control plane (it cannot reach the control plane's
//! public URL, if there is one) talks to the control plane over the mesh:
//! `https://<control-plane mesh address>:<node-api port>`. This listener
//! serves only the routes nodes call, under `/api/internal/nodes/`, over TLS
//! with a certificate from the cluster CA for the mesh address. WireGuard
//! authenticates both ends; TLS keeps the agent's credentials private on
//! paths that cross a hub.
//!
//! The listener follows the mesh settings: it binds once the control plane's
//! end of the mesh is up, and binds again when the pool or port changes.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Weak};
use std::time::Duration;

use axum::{
    body::Body,
    extract::{ConnectInfo, Request},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
    Router,
};
use sea_orm::DatabaseConnection;
use temps_config::ConfigService;
use temps_core::EncryptionService;
use tracing::{info, warn};

/// How often the listener re-reads the mesh settings.
const SETTINGS_POLL: Duration = Duration::from_secs(10);
/// Pause after a failed `accept` (e.g. out of file descriptors), so the
/// listener does not spin on an error that persists.
const ACCEPT_ERROR_BACKOFF: Duration = Duration::from_millis(100);
/// How long a connection has to finish its TLS handshake.
const TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// How long an HTTP/1 connection has to send a request's headers. hyper
/// arms it whenever it waits for a request, so it also closes a keep-alive
/// connection left idle this long.
const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(30);
/// HTTP/2 keep-alive: ping an idle connection this often, and close it
/// when a ping goes unanswered this long, so a vanished peer does not hold
/// a connection slot.
const HTTP2_KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(30);
const HTTP2_KEEP_ALIVE_TIMEOUT: Duration = Duration::from_secs(10);
/// Requests in flight on one HTTP/2 connection. An agent makes a handful.
const HTTP2_MAX_CONCURRENT_STREAMS: u32 = 32;
/// Connections served at once, TLS handshake included. Every node holds a
/// few (heartbeat, peer sync, routes); past this a new connection is closed
/// on accept, so a misbehaving member cannot exhaust the control plane's
/// tasks and file descriptors.
const MAX_CONNECTIONS: usize = 1024;
/// One mesh member cannot occupy every listener slot.
const MAX_PEER_CONNECTIONS: usize = 16;
/// At most one warning per this interval about connections refused at the
/// limit, carrying how many were refused since the last one.
const REFUSED_LOG_INTERVAL: Duration = Duration::from_secs(60);

/// Serve `api` (the console's `/api` routes) to mesh members for the life of
/// the process.
pub(crate) fn spawn(
    db: Arc<DatabaseConnection>,
    config_service: Arc<ConfigService>,
    encryption_service: Arc<EncryptionService>,
    api: Router,
) {
    let app = api.layer(axum::middleware::from_fn(only_node_routes));
    tokio::spawn(async move {
        let mut connections = ConnectionLimits::default();
        loop {
            let bound = match current_address(&db).await {
                Some(address) => {
                    serve_while_unchanged(
                        &db,
                        &config_service,
                        &encryption_service,
                        &app,
                        address,
                        &mut connections,
                    )
                    .await
                }
                None => false,
            };
            if !bound {
                tokio::time::sleep(SETTINGS_POLL).await;
            }
        }
    });
}

/// Where the node API belongs now: `None` while the mesh is off or the
/// control plane's end is not up.
async fn current_address(db: &DatabaseConnection) -> Option<SocketAddr> {
    let settings = match temps_network::mesh::load_settings(db).await {
        Ok(settings) => settings?,
        Err(error) => {
            warn!(%error, "node API: could not read the mesh settings");
            return None;
        }
    };
    match temps_network::mesh::published_control_plane(db).await {
        Ok(Some(_)) => Some(SocketAddr::new(
            IpAddr::V4(settings.control_plane_address()),
            settings.node_api_port,
        )),
        Ok(None) => None,
        Err(error) => {
            warn!(%error, "node API: could not read the control plane's mesh state");
            None
        }
    }
}

/// Serve on `address` until the mesh settings move it. Returns whether it
/// got as far as binding (so a bind failure is retried after a pause).
async fn serve_while_unchanged(
    db: &DatabaseConnection,
    config_service: &ConfigService,
    encryption_service: &EncryptionService,
    app: &Router,
    address: SocketAddr,
    connections: &mut ConnectionLimits,
) -> bool {
    let tls = match tls_config(config_service, encryption_service, address.ip()).await {
        Ok(tls) => tls,
        Err(error) => {
            warn!(%error, "node API: could not issue its certificate");
            return false;
        }
    };
    // The mesh interface may still be coming up: its address is not bindable
    // until it is.
    let listener = match tokio::net::TcpListener::bind(address).await {
        Ok(listener) => listener,
        Err(error) => {
            warn!(%address, %error, "node API: cannot listen on the mesh address yet");
            return false;
        }
    };
    info!(%address, "node API listening on the mesh");
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(tls));
    // One interval for the life of the listener: a timer recreated on every
    // turn of the loop would restart with each accepted connection, and a
    // steady stream of connections would keep the settings from being read.
    let mut settings_poll =
        tokio::time::interval_at(tokio::time::Instant::now() + SETTINGS_POLL, SETTINGS_POLL);
    settings_poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut refused = RefusedConnections::default();
    loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, peer)) => match connections.acquire(peer.ip()) {
                    Some(permit) => {
                        serve_connection(acceptor.clone(), app.clone(), stream, peer, permit)
                    }
                    None => {
                        // Closed at once: no task, no TLS handshake.
                        drop(stream);
                        if let Some(count) = refused.record(std::time::Instant::now()) {
                            warn!(
                                %address,
                                %peer,
                                refused = count,
                                limit = MAX_CONNECTIONS,
                                "node API: at its connection limit; refusing new connections"
                            );
                        }
                    }
                },
                Err(error) => {
                    warn!(%error, "node API: accept failed");
                    tokio::time::sleep(ACCEPT_ERROR_BACKOFF).await;
                }
            },
            _ = settings_poll.tick() => {
                if current_address(db).await != Some(address) {
                    info!(%address, "node API: the mesh settings changed; moving the listener");
                    return true;
                }
            }
        }
    }
}

/// The accept loop owns this bounded map; connection tasks only own permits.
struct ConnectionLimits {
    global: Arc<tokio::sync::Semaphore>,
    peers: HashMap<IpAddr, Weak<tokio::sync::Semaphore>>,
}

struct ConnectionPermits {
    _global: tokio::sync::OwnedSemaphorePermit,
    _peer: tokio::sync::OwnedSemaphorePermit,
}

impl Default for ConnectionLimits {
    fn default() -> Self {
        Self {
            global: Arc::new(tokio::sync::Semaphore::new(MAX_CONNECTIONS)),
            peers: HashMap::new(),
        }
    }
}

impl ConnectionLimits {
    fn acquire(&mut self, ip: IpAddr) -> Option<ConnectionPermits> {
        let global = self.global.clone().try_acquire_owned().ok()?;
        let peer = match self.peers.get(&ip).and_then(Weak::upgrade) {
            Some(peer) => peer,
            None => {
                // Only active connections keep entries alive. Sweep at the
                // bound, rather than scanning every entry on every accept.
                if self.peers.len() >= MAX_CONNECTIONS {
                    self.peers.retain(|_, peer| peer.strong_count() > 0);
                }
                let peer = Arc::new(tokio::sync::Semaphore::new(MAX_PEER_CONNECTIONS));
                self.peers.insert(ip, Arc::downgrade(&peer));
                peer
            }
        };
        Some(ConnectionPermits {
            _global: global,
            _peer: peer.try_acquire_owned().ok()?,
        })
    }
}

/// Connections refused at [`MAX_CONNECTIONS`], logged at most once per
/// [`REFUSED_LOG_INTERVAL`] so a flood does not become a log flood.
#[derive(Debug, Default)]
struct RefusedConnections {
    /// Refused since the last warning.
    unlogged: u64,
    last_logged: Option<std::time::Instant>,
}

impl RefusedConnections {
    /// Count a refused connection. Returns how many to report when a
    /// warning is due now.
    fn record(&mut self, now: std::time::Instant) -> Option<u64> {
        self.unlogged = self.unlogged.saturating_add(1);
        let due = self
            .last_logged
            .is_none_or(|last| now.saturating_duration_since(last) >= REFUSED_LOG_INTERVAL);
        if !due {
            return None;
        }
        self.last_logged = Some(now);
        Some(std::mem::take(&mut self.unlogged))
    }
}

/// The HTTP server for one connection: HTTP/1 must send its headers within
/// `header_read_timeout` ([`HEADER_READ_TIMEOUT`] in production), HTTP/2
/// must answer keep-alive pings, and neither runs without a timer (hyper
/// skips both without one).
fn http_builder(
    header_read_timeout: Duration,
) -> hyper_util::server::conn::auto::Builder<hyper_util::rt::TokioExecutor> {
    use hyper_util::rt::{TokioExecutor, TokioTimer};

    let mut builder = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new());
    builder
        .http1()
        .timer(TokioTimer::new())
        .header_read_timeout(header_read_timeout);
    builder
        .http2()
        .timer(TokioTimer::new())
        .keep_alive_interval(HTTP2_KEEP_ALIVE_INTERVAL)
        .keep_alive_timeout(HTTP2_KEEP_ALIVE_TIMEOUT)
        .max_concurrent_streams(HTTP2_MAX_CONCURRENT_STREAMS);
    builder
}

/// Serve one accepted connection. `permit` is its slot under
/// [`MAX_CONNECTIONS`], released when the connection ends.
fn serve_connection(
    acceptor: tokio_rustls::TlsAcceptor,
    app: Router,
    stream: tokio::net::TcpStream,
    peer: SocketAddr,
    permit: ConnectionPermits,
) {
    use hyper_util::rt::TokioIo;
    use tower::Service;

    tokio::spawn(async move {
        let _permit = permit;
        let tls = match tokio::time::timeout(TLS_HANDSHAKE_TIMEOUT, acceptor.accept(stream)).await {
            Ok(Ok(tls)) => tls,
            Ok(Err(error)) => {
                warn!(%peer, %error, "node API: TLS handshake failed");
                return;
            }
            Err(_) => {
                warn!(%peer, "node API: TLS handshake timed out");
                return;
            }
        };
        let service = hyper::service::service_fn(
            move |mut request: hyper::Request<hyper::body::Incoming>| {
                mark_node_api_request(request.extensions_mut(), peer);
                let mut app = app.clone();
                async move { app.call(request.map(Body::new)).await }
            },
        );
        if let Err(error) = http_builder(HEADER_READ_TIMEOUT)
            .serve_connection(TokioIo::new(tls), service)
            .await
        {
            warn!(%peer, %error, "node API: connection error");
        }
    });
}

/// Record on a request served by this listener the TCP peer it came from:
/// as `ConnectInfo` (what the handlers' rate limiting and logs read), and as
/// [`NodeApiPeer`], which tells the registration handler the request arrived
/// over the mesh node API so it can accept a pairing's enrollment token from
/// the pairing's reserved mesh address only. Both come from the accepted
/// socket; nothing the client sends can set them.
///
/// [`NodeApiPeer`]: temps_deployments::handlers::nodes::NodeApiPeer
fn mark_node_api_request(extensions: &mut axum::http::Extensions, peer: SocketAddr) {
    extensions.insert(ConnectInfo(peer));
    extensions.insert(temps_deployments::handlers::nodes::NodeApiPeer(peer));
}

/// A server certificate for the control plane's mesh address, from the
/// cluster CA, presented with the CA so a node pinning the CA's fingerprint
/// can verify it before it holds the CA.
async fn tls_config(
    config_service: &ConfigService,
    encryption_service: &EncryptionService,
    address: IpAddr,
) -> Result<rustls::ServerConfig, String> {
    let ca = temps_config::cluster_ca::ensure_cluster_ca(config_service, encryption_service)
        .await
        .map_err(|error| error.to_string())?;
    server_config(&ca.cert_pem, &ca.key_pem, address)
}

/// A TLS server config presenting a fresh leaf, signed by the CA in
/// `ca_cert_pem`/`ca_key_pem`, followed by the CA itself. The leaf is for the
/// reserved control-plane name, which nodes verify, and for `address`, which
/// nodes enrolled before the reserved name existed still verify.
fn server_config(
    ca_cert_pem: &str,
    ca_key_pem: &str,
    address: IpAddr,
) -> Result<rustls::ServerConfig, String> {
    use rustls::pki_types::{CertificateDer, PrivateKeyDer};

    let sans = temps_core::node_pki::control_plane_node_api_sans(address);
    let csr = temps_core::node_pki::generate_node_keypair_csr("temps-control-plane", &sans)
        .map_err(|error| error.to_string())?;
    let signed = temps_core::node_pki::sign_node_csr(ca_cert_pem, ca_key_pem, &csr.csr_pem, &sans)
        .map_err(|error| error.to_string())?;
    let mut chain: Vec<CertificateDer<'static>> = Vec::new();
    for pem in [signed.cert_pem.as_str(), ca_cert_pem] {
        for cert in rustls_pemfile::certs(&mut pem.as_bytes()) {
            chain.push(cert.map_err(|error| error.to_string())?);
        }
    }
    let key: PrivateKeyDer<'static> = rustls_pemfile::private_key(&mut csr.key_pem.as_bytes())
        .map_err(|error| error.to_string())?
        .ok_or("the generated key is not PEM")?;
    // Name the provider rather than rely on a process-wide default, which
    // only `temps serve` installs and which rustls cannot infer when more
    // than one provider is compiled in.
    let provider = std::sync::Arc::new(rustls::crypto::ring::default_provider());
    let mut config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|error| error.to_string())?
        .with_no_client_auth()
        .with_single_cert(chain, key)
        .map_err(|error| error.to_string())?;
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(config)
}

/// Whether `path` is a route nodes call. Everything else the console serves
/// stays off the mesh.
///
/// Compares the raw request path: nothing on this listener normalizes paths,
/// so an encoded or dotted variant fails the match and is refused. Adding a
/// path-normalizing layer in front of this check would require comparing the
/// normalized path instead. Ids must be plain digits, so every segment is
/// either a fixed word or a number.
fn is_node_route(path: &str) -> bool {
    let Some(rest) = path.strip_prefix("/api/internal/nodes/") else {
        return false;
    };
    if rest == "register" {
        return true;
    }
    let Some((node_id, route)) = rest.split_once('/') else {
        return false;
    };
    is_id(node_id)
        && (matches!(
            route,
            "heartbeat"
                | "network/peers"
                | "network/wireguard"
                | "network/wireguard/handshakes"
                | "routes/snapshot"
                | "routes/ack"
                | "acme-challenge"
        ) || route.strip_prefix("s3-credentials/").is_some_and(is_id))
}

/// A database id as it appears in a path: ASCII digits that fit an `i32`
/// (no sign, no percent-encoding).
fn is_id(segment: &str) -> bool {
    !segment.is_empty()
        && segment.bytes().all(|byte| byte.is_ascii_digit())
        && segment.parse::<i32>().is_ok()
}

async fn only_node_routes(request: Request, next: Next) -> Response {
    if is_node_route(request.uri().path()) {
        next.run(request).await
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{Method, Request as HttpRequest};
    use tower::ServiceExt;

    #[test]
    fn one_peer_cannot_exhaust_the_listener_and_slots_are_released() {
        let mut limits = ConnectionLimits::default();
        let noisy: IpAddr = "10.201.0.2".parse().unwrap();
        let other: IpAddr = "10.201.0.3".parse().unwrap();
        let permits: Vec<_> = (0..MAX_PEER_CONNECTIONS)
            .map(|_| limits.acquire(noisy).unwrap())
            .collect();
        assert!(limits.acquire(noisy).is_none());
        assert!(limits.acquire(other).is_some());
        drop(permits);
        assert!(limits.acquire(noisy).is_some());
        for ip in 1..=2048u32 {
            assert!(limits.acquire(IpAddr::V4(ip.into())).is_some());
        }
        assert!(limits.peers.len() <= MAX_CONNECTIONS);
    }

    #[test]
    fn refused_connections_are_counted_and_logged_at_a_bounded_rate() {
        let start = std::time::Instant::now();
        let mut refused = RefusedConnections::default();
        // The first one is reported at once.
        assert_eq!(refused.record(start), Some(1));
        // A flood within the interval is only counted.
        for i in 1..=500 {
            assert_eq!(refused.record(start + Duration::from_millis(i)), None);
        }
        // The next warning carries everything refused since the last.
        assert_eq!(refused.record(start + REFUSED_LOG_INTERVAL), Some(501));
        assert_eq!(
            refused.record(start + REFUSED_LOG_INTERVAL + Duration::from_secs(1)),
            None
        );
    }

    /// Serve one plain-TCP connection with the node API's HTTP settings.
    async fn one_connection(
        header_read_timeout: Duration,
    ) -> (SocketAddr, tokio::task::JoinHandle<()>) {
        use hyper_util::rt::TokioIo;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let service = hyper::service::service_fn(|_request| async {
                Ok::<_, std::convert::Infallible>(hyper::Response::new(Body::from("ok")))
            });
            let _ = http_builder(header_read_timeout)
                .serve_connection(TokioIo::new(stream), service)
                .await;
        });
        (address, server)
    }

    #[tokio::test]
    async fn a_connection_that_never_finishes_its_headers_is_closed() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (address, server) = one_connection(Duration::from_millis(200)).await;
        let mut client = tokio::net::TcpStream::connect(address).await.unwrap();
        client
            .write_all(b"GET /api/internal/nodes/1/heartbeat HTTP/1.1\r\nHost: x\r\n")
            .await
            .unwrap();
        let mut response = Vec::new();
        tokio::time::timeout(Duration::from_secs(10), client.read_to_end(&mut response))
            .await
            .expect("the server kept a connection with unfinished headers open")
            .ok();
        tokio::time::timeout(Duration::from_secs(10), server)
            .await
            .expect("the connection task did not end")
            .unwrap();
    }

    #[tokio::test]
    async fn a_complete_request_is_still_served() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (address, _server) = one_connection(Duration::from_millis(200)).await;
        let mut client = tokio::net::TcpStream::connect(address).await.unwrap();
        client
            .write_all(b"GET / HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut response = Vec::new();
        tokio::time::timeout(Duration::from_secs(10), client.read_to_end(&mut response))
            .await
            .unwrap()
            .unwrap();
        assert!(response.starts_with(b"HTTP/1.1 200"), "{response:?}");
    }

    /// The node API's filter in front of a router shaped like the console's:
    /// node routes, admin routes on the same prefix, and a fallback, so a
    /// request the filter lets through never comes back 404.
    fn mesh_app() -> Router {
        use axum::routing::{get, post, put};
        let api = Router::new()
            .route("/internal/nodes/register", post(|| async { "register" }))
            .route(
                "/internal/nodes/{node_id}/heartbeat",
                post(|| async { "heartbeat" }),
            )
            .route(
                "/internal/nodes/{node_id}/network/wireguard",
                put(|| async { "wireguard" }),
            )
            .route(
                "/internal/nodes/{node_id}/s3-credentials/{source_id}",
                get(|| async { "s3" }),
            )
            .route("/internal/nodes", get(|| async { "admin list" }))
            .route(
                "/internal/nodes/{node_id}",
                get(|| async { "admin get" }).delete(|| async { "admin delete" }),
            )
            .route(
                "/internal/nodes/{node_id}/drain",
                post(|| async { "admin drain" }),
            )
            .route("/settings", get(|| async { "settings" }))
            .fallback(|| async { (StatusCode::IM_A_TEAPOT, "not filtered") });
        Router::new()
            .nest("/api", api)
            .layer(axum::middleware::from_fn(only_node_routes))
    }

    async fn status(method: Method, uri: &str) -> StatusCode {
        mesh_app()
            .oneshot(
                HttpRequest::builder()
                    .method(method)
                    .uri(uri)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
    }

    #[tokio::test]
    async fn the_mesh_listener_serves_node_routes() {
        for (method, uri) in [
            (Method::POST, "/api/internal/nodes/register"),
            (Method::POST, "/api/internal/nodes/7/heartbeat"),
            (Method::PUT, "/api/internal/nodes/7/network/wireguard"),
            (Method::GET, "/api/internal/nodes/7/s3-credentials/2"),
            (
                Method::POST,
                "/api/internal/nodes/7/heartbeat?after=../drain",
            ),
        ] {
            assert_eq!(
                status(method.clone(), uri).await,
                StatusCode::OK,
                "{method} {uri}"
            );
        }
    }

    #[tokio::test]
    async fn the_mesh_listener_refuses_everything_else() {
        for (method, uri) in [
            // Admin routes, including on another node's id.
            (Method::GET, "/api/internal/nodes"),
            (Method::GET, "/api/internal/nodes/8"),
            (Method::DELETE, "/api/internal/nodes/8"),
            (Method::POST, "/api/internal/nodes/8/drain"),
            (Method::GET, "/api/settings"),
            // Dotted and encoded escapes from an allowed prefix.
            (Method::POST, "/api/internal/nodes/7/../8/drain"),
            (
                Method::POST,
                "/api/internal/nodes/7/heartbeat/../../8/drain",
            ),
            (
                Method::GET,
                "/api/internal/nodes/7/s3-credentials/../../8/drain",
            ),
            (Method::POST, "/api/internal/nodes/7/%2e%2e/8/drain"),
            (Method::POST, "/api/internal/nodes/7/%2E%2E/8/drain"),
            (Method::POST, "/api/internal/nodes/7%2Fdrain"),
            (Method::POST, "/api/internal/nodes/7/heartbeat%2F..%2Fdrain"),
            (Method::POST, "/api/internal/nodes/%37/heartbeat"),
            // Doubled and trailing slashes.
            (Method::POST, "/api/internal/nodes//7/heartbeat"),
            (Method::POST, "/api/internal/nodes/7//heartbeat"),
            (Method::POST, "//api/internal/nodes/7/heartbeat"),
            (Method::POST, "/api/internal/nodes/7/heartbeat/"),
            (Method::POST, "/api/internal/nodes/register/"),
            // Ids that are not plain digits.
            (Method::POST, "/api/internal/nodes/+7/heartbeat"),
            (Method::POST, "/api/internal/nodes/-7/heartbeat"),
            (Method::POST, "/api/internal/nodes/99999999999/heartbeat"),
            (Method::GET, "/api/internal/nodes/7/s3-credentials/"),
            (Method::GET, "/api/internal/nodes/7/s3-credentials/2/x"),
        ] {
            assert_eq!(
                status(method.clone(), uri).await,
                StatusCode::NOT_FOUND,
                "{method} {uri}"
            );
        }
    }

    /// The filter allows paths, not methods: routing answers a wrong method,
    /// and no admin route shares a path with a node route.
    #[tokio::test]
    async fn a_node_route_with_the_wrong_method_reaches_no_handler() {
        assert_eq!(
            status(Method::GET, "/api/internal/nodes/7/heartbeat").await,
            StatusCode::METHOD_NOT_ALLOWED
        );
        assert_eq!(
            status(Method::DELETE, "/api/internal/nodes/register").await,
            StatusCode::METHOD_NOT_ALLOWED
        );
    }

    /// The node API presents its leaf and then the cluster CA, so a node that
    /// pins the CA's fingerprint finds it in the chain, and the leaf verifies
    /// against that CA for the mesh address.
    #[tokio::test]
    async fn the_node_api_presents_a_leaf_for_its_address_and_the_cluster_ca() {
        use rustls::pki_types::{CertificateDer, ServerName};
        use sha2::Digest;

        // `temps` installs this at startup; a test process has to do it itself,
        // since rustls cannot pick one when both ring and aws-lc-rs are linked.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let ca = temps_core::node_pki::generate_cluster_ca().unwrap();
        let address: IpAddr = "127.0.0.1".parse().unwrap();
        let config = server_config(&ca.cert_pem, &ca.key_pem, address).unwrap();
        let listener = tokio::net::TcpListener::bind((address, 0)).await.unwrap();
        let bound = listener.local_addr().unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let _tls = acceptor.accept(stream).await.unwrap();
        });

        let ca_der: CertificateDer<'static> = rustls_pemfile::certs(&mut ca.cert_pem.as_bytes())
            .next()
            .unwrap()
            .unwrap();
        let mut roots = rustls::RootCertStore::empty();
        roots.add(ca_der.clone()).unwrap();
        let client = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
        let stream = tokio::net::TcpStream::connect(bound).await.unwrap();
        let tls = tokio_rustls::TlsConnector::from(Arc::new(client))
            .connect(ServerName::IpAddress(address.into()), stream)
            .await
            .expect("the leaf verifies against the cluster CA for the mesh address");
        let chain = tls.get_ref().1.peer_certificates().unwrap().to_vec();
        assert_eq!(chain.len(), 2, "leaf, then the CA");
        assert_eq!(chain[1], ca_der);
        assert_eq!(
            hex::encode(sha2::Sha256::digest(&chain[1])),
            temps_core::node_pki::ca_fingerprint_sha256(&ca.cert_pem).unwrap()
        );
        server.await.unwrap();
    }

    #[test]
    fn requests_on_the_node_api_carry_their_tcp_peer() {
        let peer: SocketAddr = "10.201.0.7:40312".parse().unwrap();
        let mut extensions = axum::http::Extensions::new();
        mark_node_api_request(&mut extensions, peer);
        assert_eq!(
            extensions
                .get::<ConnectInfo<SocketAddr>>()
                .map(|info| info.0),
            Some(peer)
        );
        assert_eq!(
            extensions.get::<temps_deployments::handlers::nodes::NodeApiPeer>(),
            Some(&temps_deployments::handlers::nodes::NodeApiPeer(peer))
        );
    }

    /// Nodes verify the node API by the reserved control-plane name, not by
    /// the mesh address they connect to, so a worker's leaf for that address
    /// is not accepted in its place (see `node_pki::CONTROL_PLANE_SERVER_NAME`).
    #[tokio::test]
    async fn nodes_verify_the_node_api_by_the_reserved_name() {
        use rustls::pki_types::ServerName;

        let _ = rustls::crypto::ring::default_provider().install_default();
        let ca = temps_core::node_pki::generate_cluster_ca().unwrap();
        let address: IpAddr = "127.0.0.1".parse().unwrap();
        let config = server_config(&ca.cert_pem, &ca.key_pem, address).unwrap();
        let listener = tokio::net::TcpListener::bind((address, 0)).await.unwrap();
        let bound = listener.local_addr().unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let _tls = acceptor.accept(stream).await.unwrap();
        });

        let client =
            temps_core::node_pki::control_plane_client_config(ca.cert_pem.as_bytes()).unwrap();
        let stream = tokio::net::TcpStream::connect(bound).await.unwrap();
        tokio_rustls::TlsConnector::from(Arc::new(client))
            .connect(ServerName::IpAddress(address.into()), stream)
            .await
            .expect("the node API's leaf carries the reserved control-plane name");
        server.await.unwrap();
    }

    #[test]
    fn the_node_api_config_needs_the_ca_key() {
        let ca = temps_core::node_pki::generate_cluster_ca().unwrap();
        let address: IpAddr = "10.201.0.1".parse().unwrap();
        assert!(server_config(&ca.cert_pem, "not a key", address).is_err());
        assert!(server_config("not a certificate", &ca.key_pem, address).is_err());
    }

    #[test]
    fn only_routes_nodes_call_are_served_on_the_mesh() {
        for allowed in [
            "/api/internal/nodes/register",
            "/api/internal/nodes/7/heartbeat",
            "/api/internal/nodes/7/network/peers",
            "/api/internal/nodes/7/network/wireguard",
            "/api/internal/nodes/7/network/wireguard/handshakes",
            "/api/internal/nodes/7/routes/snapshot",
            "/api/internal/nodes/7/routes/ack",
            "/api/internal/nodes/7/acme-challenge",
            "/api/internal/nodes/7/s3-credentials/2",
        ] {
            assert!(is_node_route(allowed), "{allowed}");
        }
        for denied in [
            "/api/internal/nodes",
            "/api/internal/nodes/7",
            "/api/internal/nodes/7/drain",
            "/api/internal/nodes/7/containers",
            "/api/internal/nodes/abc/heartbeat",
            "/api/nodes/pairings",
            "/api/settings",
            "/",
        ] {
            assert!(!is_node_route(denied), "{denied}");
        }
    }
}
