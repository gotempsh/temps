// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The control plane's tunnel origin (ADR-107 D3).
//!
//! A connector the control plane runs (a `cloudflared` placed on node `0`)
//! forwards plain HTTP to a loopback listener the proxy binds at startup.
//! Requests on that listener came through the tunnel: the visitor reached
//! the provider over HTTPS, and the provider's client-address header is
//! trusted there, and only there.
//!
//! The proxy binds the listener and reads [`ControlPlaneTunnelOrigin`] per
//! request (atomics and an `ArcSwap`, no lock); the control plane's connector
//! loop announces which header its connectors put the visitor's address in.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::Arc;

use arc_swap::ArcSwapOption;
use axum::http::{HeaderMap, HeaderName};

/// The loopback port of the tunnel origin: the default `edge_port` of every
/// connector, so one tunnel configuration (`http://127.0.0.1:7480`) reaches
/// the origin on a worker and on the control plane alike.
pub const TUNNEL_ORIGIN_PORT: u16 = 7480;

/// The last port the control plane's proxy tries when the ones before it
/// are taken. A worker agent on the same machine holds
/// [`TUNNEL_ORIGIN_PORT`] for its own connectors; the control plane then
/// takes the next free one, and a tunnel placed only on the control plane
/// follows it (ADR-107 D3).
pub const TUNNEL_ORIGIN_LAST_PORT: u16 = 7489;

/// Where the control plane's proxy listens for its connectors when the
/// port is free.
pub fn tunnel_origin_address() -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), TUNNEL_ORIGIN_PORT)
}

/// Every loopback address the control plane's proxy may take for its
/// tunnel origin, in the order it tries them.
pub fn tunnel_origin_candidates() -> impl Iterator<Item = SocketAddr> {
    (TUNNEL_ORIGIN_PORT..=TUNNEL_ORIGIN_LAST_PORT)
        .map(|port| SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port))
}

/// What the proxy and the control plane's connectors share about the tunnel
/// origin.
#[derive(Debug, Default)]
pub struct ControlPlaneTunnelOrigin {
    /// The bound port, `0` while nothing listens.
    port: AtomicU16,
    /// The header carrying the visitor's address, announced by the running
    /// connectors. `None`: the TCP peer is the client.
    client_address_header: ArcSwapOption<HeaderName>,
}

/// Why a client-address header announced by a connector was refused.
#[derive(Debug, thiserror::Error)]
#[error("connector client-address header '{header}' is not a valid HTTP header name")]
pub struct InvalidClientAddressHeader {
    pub header: String,
}

impl ControlPlaneTunnelOrigin {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Record that the proxy listens on `port` (loopback).
    pub fn mark_bound(&self, port: u16) {
        self.port.store(port, Ordering::Release);
    }

    /// The port the proxy listens on, `None` when it could not bind one.
    pub fn bound_port(&self) -> Option<u16> {
        match self.port.load(Ordering::Acquire) {
            0 => None,
            port => Some(port),
        }
    }

    /// Whether a connection accepted on `local` came in on the tunnel origin.
    pub fn is_origin(&self, local: SocketAddr) -> bool {
        let port = self.port.load(Ordering::Relaxed);
        port != 0 && local.port() == port && is_loopback(local.ip())
    }

    /// Set the header the running connectors carry the visitor's address in.
    pub fn set_client_address_header(
        &self,
        header: Option<&str>,
    ) -> Result<(), InvalidClientAddressHeader> {
        let parsed = header
            .map(|name| {
                HeaderName::from_bytes(name.trim().as_bytes()).map_err(|_| {
                    InvalidClientAddressHeader {
                        header: name.to_string(),
                    }
                })
            })
            .transpose()?;
        let unchanged = match (self.client_address_header.load().as_deref(), &parsed) {
            (Some(current), Some(new)) => current == new,
            (None, None) => true,
            _ => false,
        };
        if !unchanged {
            self.client_address_header.store(parsed.map(Arc::new));
        }
        Ok(())
    }

    /// The visitor's address for a request on the tunnel origin: the
    /// announced header when it holds one address, otherwise the TCP peer.
    pub fn client_ip(&self, peer: IpAddr, headers: &HeaderMap) -> IpAddr {
        let guard = self.client_address_header.load();
        let Some(name) = guard.as_deref() else {
            return peer;
        };
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.trim().parse::<IpAddr>().ok())
            .unwrap_or(peer)
    }
}

/// Loopback, including the IPv4-mapped form a dual-stack socket reports.
fn is_loopback(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback(),
        IpAddr::V6(v6) => {
            v6.is_loopback() || v6.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(
                HeaderName::from_bytes(name.as_bytes()).unwrap(),
                value.parse().unwrap(),
            );
        }
        map
    }

    #[test]
    fn nothing_is_the_origin_until_the_proxy_binds_it() {
        let origin = ControlPlaneTunnelOrigin::new();
        assert!(!origin.is_origin(tunnel_origin_address()));
        assert_eq!(origin.bound_port(), None);
        origin.mark_bound(TUNNEL_ORIGIN_PORT);
        assert!(origin.is_origin(tunnel_origin_address()));
        assert_eq!(origin.bound_port(), Some(TUNNEL_ORIGIN_PORT));
    }

    #[test]
    fn only_the_loopback_listener_on_that_port_is_the_origin() {
        let origin = ControlPlaneTunnelOrigin::new();
        origin.mark_bound(TUNNEL_ORIGIN_PORT);
        assert!(origin.is_origin("[::ffff:127.0.0.1]:7480".parse().unwrap()));
        assert!(!origin.is_origin("127.0.0.1:80".parse().unwrap()));
        assert!(!origin.is_origin("192.0.2.10:7480".parse().unwrap()));
        assert!(!origin.is_origin("0.0.0.0:7480".parse().unwrap()));
    }

    #[test]
    fn the_client_address_comes_from_the_announced_header_only() {
        let origin = ControlPlaneTunnelOrigin::new();
        let peer: IpAddr = "127.0.0.1".parse().unwrap();
        let request = headers(&[
            ("cf-connecting-ip", "198.51.100.23"),
            ("x-forwarded-for", "192.0.2.99"),
        ]);
        // Nothing announced yet: the peer, whatever the request says.
        assert_eq!(origin.client_ip(peer, &request), peer);

        origin
            .set_client_address_header(Some("CF-Connecting-IP"))
            .unwrap();
        assert_eq!(
            origin.client_ip(peer, &request),
            "198.51.100.23".parse::<IpAddr>().unwrap()
        );

        // A missing or unparsable header falls back to the peer, never to
        // another header.
        assert_eq!(
            origin.client_ip(peer, &headers(&[("x-forwarded-for", "192.0.2.99")])),
            peer
        );
        assert_eq!(
            origin.client_ip(peer, &headers(&[("cf-connecting-ip", "not-an-ip")])),
            peer
        );

        origin.set_client_address_header(None).unwrap();
        assert_eq!(origin.client_ip(peer, &request), peer);
    }

    #[test]
    fn an_invalid_header_name_is_refused_and_keeps_the_current_one() {
        let origin = ControlPlaneTunnelOrigin::new();
        origin
            .set_client_address_header(Some("CF-Connecting-IP"))
            .unwrap();
        let error = origin
            .set_client_address_header(Some("bad header"))
            .unwrap_err();
        assert!(error.to_string().contains("bad header"), "{error}");
        let request = headers(&[("cf-connecting-ip", "198.51.100.23")]);
        assert_eq!(
            origin.client_ip("127.0.0.1".parse().unwrap(), &request),
            "198.51.100.23".parse::<IpAddr>().unwrap()
        );
    }
}
