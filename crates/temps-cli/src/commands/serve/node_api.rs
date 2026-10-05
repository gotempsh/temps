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

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
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
        loop {
            let bound = match current_address(&db).await {
                Some(address) => {
                    serve_while_unchanged(&db, &config_service, &encryption_service, &app, address)
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
    loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, peer)) => serve_connection(acceptor.clone(), app.clone(), stream, peer),
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

fn serve_connection(
    acceptor: tokio_rustls::TlsAcceptor,
    app: Router,
    stream: tokio::net::TcpStream,
    peer: SocketAddr,
) {
    use hyper_util::rt::{TokioExecutor, TokioIo};
    use tower::Service;

    tokio::spawn(async move {
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
                request.extensions_mut().insert(ConnectInfo(peer));
                let mut app = app.clone();
                async move { app.call(request.map(Body::new)).await }
            },
        );
        if let Err(error) = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new())
            .serve_connection(TokioIo::new(tls), service)
            .await
        {
            warn!(%peer, %error, "node API: connection error");
        }
    });
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

/// A TLS server config presenting a fresh leaf for `address`, signed by the
/// CA in `ca_cert_pem`/`ca_key_pem`, followed by the CA itself.
fn server_config(
    ca_cert_pem: &str,
    ca_key_pem: &str,
    address: IpAddr,
) -> Result<rustls::ServerConfig, String> {
    use rustls::pki_types::{CertificateDer, PrivateKeyDer};

    let sans = vec![address.to_string()];
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
