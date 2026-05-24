//! Axum middleware for the admin console listener.
//!
//! The data types — `AdminGateConfig`, `AdminGateHandle`, `AdminGateSource`
//! — live in `temps_core::admin_gate` so both the proxy and the console
//! listener can share a single handle. This module just wires those types
//! into an axum middleware function that enforces them on every request
//! reaching the admin listener.
//!
//! This is defense-in-depth: the primary enforcement point is the Pingora
//! proxy, which decides whether to fall back to the console at all based
//! on the same handle. Anyone who reaches this middleware bypassed the
//! proxy (e.g. by hitting `console_address` directly on the host's
//! loopback interface), so the gate still applies here.
//!
//! Denials return `404 Not Found` rather than `403 Forbidden` so that a
//! probing client cannot tell the admin surface exists at all.

use std::net::{IpAddr, SocketAddr};

use axum::{
    extract::{ConnectInfo, Request, State},
    http::{header, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use tracing::{debug, warn};

pub use temps_core::admin_gate::{
    AdminGateConfig, AdminGateConfigError, AdminGateHandle, AdminGateSource,
};

/// Resolve the effective client IP for gating purposes. When
/// `trust_forwarded_for` is true and the immediate peer is loopback, the
/// leftmost address in `X-Forwarded-For` wins; otherwise the peer's address
/// is used directly.
fn effective_client_ip(req: &Request, peer: IpAddr, trust_forwarded_for: bool) -> IpAddr {
    if !trust_forwarded_for || !peer.is_loopback() {
        return peer;
    }
    let Some(value) = req.headers().get("x-forwarded-for") else {
        return peer;
    };
    let Ok(value) = value.to_str() else {
        return peer;
    };
    value
        .split(',')
        .next()
        .and_then(|s| s.trim().parse::<IpAddr>().ok())
        .unwrap_or(peer)
}

fn host_header(req: &Request) -> Option<String> {
    req.headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(|h| h.to_string())
}

/// Axum middleware that enforces the admin gate. Wire this onto the admin
/// router after `build_split_application` and before `axum::serve`.
pub async fn admin_gate(
    State(handle): State<AdminGateHandle>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    req: Request,
    next: Next,
) -> Response {
    let config = handle.current();
    if config.is_noop() {
        return next.run(req).await;
    }

    let client_ip = effective_client_ip(&req, peer.ip(), config.trust_forwarded_for);
    let host = host_header(&req);

    if !config.would_allow(client_ip, host.as_deref()) {
        warn!(
            client_ip = %client_ip,
            peer = %peer,
            host = host.as_deref().unwrap_or(""),
            path = %req.uri().path(),
            "admin gate denied"
        );
        return StatusCode::NOT_FOUND.into_response();
    }

    debug!(client_ip = %client_ip, path = %req.uri().path(), "admin gate allow");
    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn forwarded_for_only_trusted_from_loopback() {
        let req = Request::builder()
            .uri("/")
            .header("x-forwarded-for", "203.0.113.5")
            .body(axum::body::Body::empty())
            .unwrap();

        let peer_loopback = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let peer_external = IpAddr::V4(Ipv4Addr::new(198, 51, 100, 1));

        // Loopback + trust → use header
        assert_eq!(
            effective_client_ip(&req, peer_loopback, true),
            IpAddr::V4(Ipv4Addr::new(203, 0, 113, 5))
        );
        // External + trust → ignore header (anti-spoofing)
        assert_eq!(
            effective_client_ip(&req, peer_external, true),
            peer_external
        );
        // Loopback + no trust → ignore header
        assert_eq!(
            effective_client_ip(&req, peer_loopback, false),
            peer_loopback
        );
    }
}
