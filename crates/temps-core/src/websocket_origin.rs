// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Origin validation for WebSocket upgrades.
//!
//! Browsers do not apply CORS to WebSocket handshakes, and `SameSite` cookies
//! are scoped to the registrable domain, so a page served from a sibling
//! subdomain can open an authenticated socket with the user's session cookie.
//! Every handler that accepts a `WebSocketUpgrade` behind cookie-capable
//! authentication must call [`validate_websocket_origin`] before upgrading.

use axum::http::{header, HeaderMap, StatusCode};
use tracing::warn;

use crate::problemdetails::{self, Problem};

/// Reject a WebSocket upgrade whose `Origin` authority differs from `Host`.
///
/// Requests without an `Origin` header (non-browser clients such as the CLI)
/// pass: they cannot be driven by a hostile page. The comparison is exact on
/// host and port (case-insensitive); a suffix or "same site" comparison would
/// reintroduce the sibling-subdomain case.
pub fn validate_websocket_origin(headers: &HeaderMap) -> Result<(), Problem> {
    let Some(origin) = headers.get(header::ORIGIN) else {
        return Ok(());
    };
    let origin = origin.to_str().map_err(|_| {
        problemdetails::new(StatusCode::FORBIDDEN)
            .with_title("Forbidden")
            .with_detail("WebSocket Origin header is invalid")
    })?;
    let parsed = url::Url::parse(origin).map_err(|_| {
        problemdetails::new(StatusCode::FORBIDDEN)
            .with_title("Forbidden")
            .with_detail("WebSocket Origin header is not allowed")
    })?;
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| {
            problemdetails::new(StatusCode::FORBIDDEN)
                .with_title("Forbidden")
                .with_detail("WebSocket Host header is required when Origin is present")
        })?;
    // Compare the authority as the browser wrote it. `Url::port()` drops a
    // scheme's default port, so `https://host:443` would otherwise look
    // portless and be rejected against `Host: host:443`.
    if raw_origin_authority(origin)
        .is_some_and(|authority| authority.eq_ignore_ascii_case(host.trim()))
    {
        return Ok(());
    }
    warn!(origin = %parsed, host = %host, "Rejected cross-origin WebSocket request");
    Err(problemdetails::new(StatusCode::FORBIDDEN)
        .with_title("Forbidden")
        .with_detail("WebSocket Origin header is not allowed"))
}

/// The `host[:port]` part of an `Origin` value, exactly as written.
///
/// `None` for anything that is not a bare `scheme://host[:port]` origin, such
/// as one carrying userinfo, so it can never match a `Host` header.
fn raw_origin_authority(origin: &str) -> Option<&str> {
    let (_, rest) = origin.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    if authority.is_empty() || authority.contains('@') {
        return None;
    }
    Some(authority)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(origin: &str, host: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(header::ORIGIN, origin.parse().unwrap());
        headers.insert(header::HOST, host.parse().unwrap());
        headers
    }

    #[test]
    fn same_authority_is_allowed() {
        assert!(validate_websocket_origin(&headers(
            "https://console.example.com",
            "console.example.com"
        ))
        .is_ok());
        assert!(validate_websocket_origin(&headers(
            "https://CONSOLE.example.com",
            "console.example.com"
        ))
        .is_ok());
    }

    #[test]
    fn foreign_origin_is_forbidden() {
        let err = validate_websocket_origin(&headers(
            "https://attacker.example.com",
            "console.example.com",
        ))
        .unwrap_err();
        assert_eq!(err.status_code, StatusCode::FORBIDDEN);
    }

    #[test]
    fn sibling_subdomain_and_other_port_are_forbidden() {
        assert!(validate_websocket_origin(&headers(
            "https://app.example.com",
            "console.example.com"
        ))
        .is_err());
        assert!(
            validate_websocket_origin(&headers("http://localhost:4000", "localhost:3000")).is_err()
        );
    }

    #[test]
    fn explicit_default_port_must_match_host_exactly() {
        assert!(validate_websocket_origin(&headers(
            "https://console.example.com:443",
            "console.example.com:443"
        ))
        .is_ok());
        assert!(validate_websocket_origin(&headers(
            "https://console.example.com:443",
            "console.example.com"
        ))
        .is_err());
        assert!(validate_websocket_origin(&headers(
            "http://console.example.com",
            "console.example.com:80"
        ))
        .is_err());
    }

    #[test]
    fn origin_with_userinfo_is_forbidden() {
        assert!(validate_websocket_origin(&headers(
            "https://user@console.example.com",
            "user@console.example.com"
        ))
        .is_err());
    }

    #[test]
    fn missing_origin_is_allowed() {
        assert!(validate_websocket_origin(&HeaderMap::new()).is_ok());
    }

    #[test]
    fn origin_without_host_is_forbidden() {
        let mut h = HeaderMap::new();
        h.insert(
            header::ORIGIN,
            "https://console.example.com".parse().unwrap(),
        );
        assert!(validate_websocket_origin(&h).is_err());
    }
}
