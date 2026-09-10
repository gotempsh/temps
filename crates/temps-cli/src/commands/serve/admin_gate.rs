// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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
    http::{header, HeaderMap, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use tracing::{debug, warn};

pub use temps_core::admin_gate::{
    AdminGateConfig, AdminGateConfigError, AdminGateHandle, AdminGateSource,
};

/// Resolve the effective client IP for gating purposes. When
/// `trust_forwarded_for` is true and the immediate peer is loopback, the
/// rightmost address in `X-Forwarded-For` wins; otherwise the peer's address
/// is used directly. Temps' trusted proxy replaces inbound XFF with that
/// canonical address, so client-supplied entries cannot influence the gate.
pub(crate) fn effective_client_ip(
    headers: &HeaderMap,
    peer: IpAddr,
    trust_forwarded_for: bool,
) -> Result<IpAddr, InvalidForwardedFor> {
    if !trust_forwarded_for || !peer.is_loopback() {
        return Ok(peer);
    }
    Ok(trusted_forwarded_client_ip(headers, peer)?.unwrap_or(peer))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct InvalidForwardedFor;

/// Return the client address supplied by a trusted loopback proxy. A present
/// but invalid header is an error rather than a fallback to loopback: treating
/// malformed proxy metadata as localhost would widen an allowlist on failure.
pub(crate) fn trusted_forwarded_client_ip(
    headers: &HeaderMap,
    peer: IpAddr,
) -> Result<Option<IpAddr>, InvalidForwardedFor> {
    if !peer.is_loopback() {
        return Ok(None);
    }
    let Some(value) = headers.get("x-forwarded-for") else {
        return Ok(None);
    };
    let value = value.to_str().map_err(|_| InvalidForwardedFor)?;
    let ip = value
        .rsplit(',')
        .next()
        .and_then(|s| s.trim().parse::<IpAddr>().ok())
        .ok_or(InvalidForwardedFor)?;
    Ok(Some(ip))
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

    let client_ip = match effective_client_ip(req.headers(), peer.ip(), config.trust_forwarded_for)
    {
        Ok(client_ip) => client_ip,
        Err(InvalidForwardedFor) => {
            warn!(
                peer = %peer,
                host = host_header(&req).as_deref().unwrap_or(""),
                path = %req.uri().path(),
                "admin gate denied request with invalid trusted X-Forwarded-For header"
            );
            return StatusCode::NOT_FOUND.into_response();
        }
    };
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
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            "198.51.100.99, 203.0.113.5".parse().unwrap(),
        );

        let peer_loopback = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let peer_external = IpAddr::V4(Ipv4Addr::new(198, 51, 100, 1));

        // Loopback + trust → use the rightmost proxy-appended address.
        assert_eq!(
            effective_client_ip(&headers, peer_loopback, true),
            Ok(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 5)))
        );
        // External + trust → ignore header (anti-spoofing)
        assert_eq!(
            effective_client_ip(&headers, peer_external, true),
            Ok(peer_external)
        );
        // Loopback + no trust → ignore header
        assert_eq!(
            effective_client_ip(&headers, peer_loopback, false),
            Ok(peer_loopback)
        );
    }

    #[test]
    fn malformed_forwarded_for_is_rejected() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "not-an-ip".parse().unwrap());
        let peer = IpAddr::V4(Ipv4Addr::LOCALHOST);

        assert_eq!(
            effective_client_ip(&headers, peer, true),
            Err(InvalidForwardedFor)
        );
    }

    #[test]
    fn missing_forwarded_for_falls_back_to_loopback_peer() {
        let headers = HeaderMap::new();
        let peer = IpAddr::V4(Ipv4Addr::LOCALHOST);

        assert_eq!(effective_client_ip(&headers, peer, true), Ok(peer));
    }

    #[test]
    fn ipv6_loopback_trusts_forwarded_ipv6_client() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "fd7a:115c:a1e0::123".parse().unwrap());
        let peer = "::1".parse::<IpAddr>().unwrap();

        assert_eq!(
            effective_client_ip(&headers, peer, true),
            Ok("fd7a:115c:a1e0::123".parse::<IpAddr>().unwrap())
        );
    }
}
