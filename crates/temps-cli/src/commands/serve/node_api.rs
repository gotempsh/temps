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
    loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, peer)) => serve_connection(acceptor.clone(), app.clone(), stream, peer),
                Err(error) => warn!(%error, "node API: accept failed"),
            },
            _ = tokio::time::sleep(SETTINGS_POLL) => {
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
        let tls = match acceptor.accept(stream).await {
            Ok(tls) => tls,
            Err(error) => {
                warn!(%peer, %error, "node API: TLS handshake failed");
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
    use rustls::pki_types::{CertificateDer, PrivateKeyDer};

    let ca = temps_config::cluster_ca::ensure_cluster_ca(config_service, encryption_service)
        .await
        .map_err(|error| error.to_string())?;
    let sans = vec![address.to_string()];
    let csr = temps_core::node_pki::generate_node_keypair_csr("temps-control-plane", &sans)
        .map_err(|error| error.to_string())?;
    let signed =
        temps_core::node_pki::sign_node_csr(&ca.cert_pem, &ca.key_pem, &csr.csr_pem, &sans)
            .map_err(|error| error.to_string())?;
    let mut chain: Vec<CertificateDer<'static>> = Vec::new();
    for pem in [&signed.cert_pem, &ca.cert_pem] {
        for cert in rustls_pemfile::certs(&mut pem.as_bytes()) {
            chain.push(cert.map_err(|error| error.to_string())?);
        }
    }
    let key: PrivateKeyDer<'static> = rustls_pemfile::private_key(&mut csr.key_pem.as_bytes())
        .map_err(|error| error.to_string())?
        .ok_or("the generated key is not PEM")?;
    let mut config = rustls::ServerConfig::builder()
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
/// normalized path instead.
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
    node_id.parse::<i32>().is_ok()
        && (matches!(
            route,
            "heartbeat"
                | "network/peers"
                | "network/wireguard"
                | "routes/snapshot"
                | "routes/ack"
                | "acme-challenge"
        ) || route.starts_with("s3-credentials/")
            || route.starts_with("acme-challenge/"))
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
    use super::is_node_route;

    #[test]
    fn only_routes_nodes_call_are_served_on_the_mesh() {
        for allowed in [
            "/api/internal/nodes/register",
            "/api/internal/nodes/7/heartbeat",
            "/api/internal/nodes/7/network/peers",
            "/api/internal/nodes/7/network/wireguard",
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
