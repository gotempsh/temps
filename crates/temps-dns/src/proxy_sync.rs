// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Proxy-owned HTTP service for worker DNS synchronization.
//!
//! The public proxy forwards only the two exact node DNS paths to this
//! loopback listener. Authentication remains in the existing handlers, which
//! validate the node bearer token against the path node id before reading or
//! mutating DNS state.

use std::{net::SocketAddr, sync::Arc};

use axum::{
    extract::{DefaultBodyLimit, Request},
    http::{header, HeaderValue},
    middleware::{self, Next},
    response::Response,
    Router,
};
use sea_orm::DatabaseConnection;
use thiserror::Error;
use tokio::net::TcpListener;
use tracing::{error, info};

use crate::{handlers, services::DnsRegistry};

async fn no_store(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

#[derive(Debug, Error)]
pub enum ProxyDnsSyncError {
    #[error("Failed to bind the proxy DNS sync listener on {address}: {source}")]
    Bind {
        address: SocketAddr,
        #[source]
        source: std::io::Error,
    },
}

/// Build the production DNS sync router without the console plugin lifecycle.
///
/// The `/api` prefix is included here because Pingora receives and forwards
/// the public request path verbatim.
pub fn proxy_dns_sync_router(db: Arc<DatabaseConnection>) -> Router {
    let state = Arc::new(handlers::dns_sync::DnsSyncAppState {
        registry: Arc::new(DnsRegistry::new(db.clone())),
        db,
    });
    Router::new()
        .nest(
            "/api",
            handlers::configure_internal_routes().with_state(state),
        )
        .layer(DefaultBodyLimit::max(4 * 1024))
        .layer(middleware::from_fn(no_store))
}

/// Start the authenticated DNS sync API on an ephemeral loopback port.
///
/// Binding completes before this returns. The spawned task lives on the
/// caller's runtime, which is the proxy control runtime in both combined and
/// split topologies.
pub async fn start_proxy_dns_sync_service(
    db: Arc<DatabaseConnection>,
) -> Result<SocketAddr, ProxyDnsSyncError> {
    let requested = SocketAddr::from(([127, 0, 0, 1], 0));
    let listener =
        TcpListener::bind(requested)
            .await
            .map_err(|source| ProxyDnsSyncError::Bind {
                address: requested,
                source,
            })?;
    let address = listener
        .local_addr()
        .map_err(|source| ProxyDnsSyncError::Bind {
            address: requested,
            source,
        })?;
    let router = proxy_dns_sync_router(db);
    tokio::spawn(async move {
        if let Err(source) = axum::serve(listener, router).await {
            error!(%address, error = %source, "Proxy DNS sync service stopped");
        }
    });
    info!(%address, "Proxy DNS sync service listening");
    Ok(address)
}
