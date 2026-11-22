// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Shared helper for resolving the true client IP from a request.
//!
//! SECURITY: `X-Forwarded-For` and `X-Real-IP` are honored ONLY when the
//! immediate TCP peer is a trusted proxy (loopback or a configured CIDR). Otherwise an attacker
//! connecting directly to the API could spoof any IP they like by setting
//! these headers, defeating audit logging and IP-based controls.
//!
//! Loopback peers (the local Pingora proxy) and DB-configured proxy ranges
//! may supply forwarded headers. The shared resolver walks the chain from
//! right to left and stops at the first untrusted hop; malformed hops retain
//! the socket peer. Direct callers cannot override their address.

use axum::http::HeaderMap;
use std::net::SocketAddr;

/// Resolve the real client IP for a request.
///
/// A missing peer produces `"unknown"`. X-Real-IP is used only when XFF is absent.
pub fn resolve_client_ip(headers: &HeaderMap, peer: Option<SocketAddr>) -> String {
    peer.map(|peer| {
        crate::trusted_proxies::resolve_trusted_proxy_ip(peer.ip(), headers, true)
            .unwrap_or(peer.ip())
            .to_string()
    })
    .unwrap_or_else(|| "unknown".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(
                axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                axum::http::HeaderValue::from_str(v).unwrap(),
            );
        }
        h
    }

    fn parse_addr(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    /// Loopback peer with multi-hop XFF → rightmost entry (proxy-appended).
    #[test]
    fn loopback_peer_xff_returns_rightmost() {
        let headers = make_headers(&[("x-forwarded-for", "1.2.3.4, 5.6.7.8")]);
        let ip = resolve_client_ip(&headers, Some(parse_addr("127.0.0.1:1234")));
        assert_eq!(ip, "5.6.7.8");
    }

    /// Non-loopback peer with XFF → peer IP wins; XFF is ignored.
    #[test]
    fn non_loopback_peer_xff_ignored() {
        let headers = make_headers(&[("x-forwarded-for", "1.2.3.4")]);
        let ip = resolve_client_ip(&headers, Some(parse_addr("8.8.8.8:443")));
        assert_eq!(ip, "8.8.8.8");
    }

    /// No peer at all → "unknown" regardless of headers.
    #[test]
    fn no_peer_returns_unknown() {
        let headers = make_headers(&[("x-forwarded-for", "1.2.3.4")]);
        let ip = resolve_client_ip(&headers, None);
        assert_eq!(ip, "unknown");
    }

    /// Loopback peer, no XFF, X-Real-IP present → X-Real-IP used.
    #[test]
    fn loopback_peer_no_xff_uses_x_real_ip() {
        let headers = make_headers(&[("x-real-ip", "9.9.9.9")]);
        let ip = resolve_client_ip(&headers, Some(parse_addr("127.0.0.1:5555")));
        assert_eq!(ip, "9.9.9.9");
    }

    /// Loopback peer, no XFF, no X-Real-IP → peer IP used.
    #[test]
    fn loopback_peer_no_headers_returns_peer_ip() {
        let headers = make_headers(&[]);
        let ip = resolve_client_ip(&headers, Some(parse_addr("127.0.0.1:5555")));
        assert_eq!(ip, "127.0.0.1");
    }

    /// IPv6 loopback peer (::1) is also trusted.
    #[test]
    fn ipv6_loopback_peer_xff_trusted() {
        let headers = make_headers(&[("x-forwarded-for", "10.0.0.1")]);
        let ip = resolve_client_ip(&headers, Some(parse_addr("[::1]:1234")));
        assert_eq!(ip, "10.0.0.1");
    }

    /// Attacker rotating XFF per request while connecting directly → always peer.
    #[test]
    fn non_loopback_attacker_cannot_rotate_xff() {
        let peer = Some(parse_addr("203.0.113.5:42424"));
        let h1 = make_headers(&[("x-forwarded-for", "1.1.1.1")]);
        let h2 = make_headers(&[("x-forwarded-for", "2.2.2.2")]);
        let h3 = make_headers(&[("x-real-ip", "9.9.9.9")]);

        assert_eq!(resolve_client_ip(&h1, peer), "203.0.113.5");
        assert_eq!(resolve_client_ip(&h2, peer), "203.0.113.5");
        assert_eq!(resolve_client_ip(&h3, peer), "203.0.113.5");
    }
}
