// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The scheme the end client used, when a trusted proxy sits in front of Temps.
//!
//! Behind a CDN the TCP connection Temps sees says nothing about what the
//! browser used. Cloudflare in "Full" mode reaches the origin over TLS even
//! for a plain `http://` visitor, so a TLS-only check never redirects them; in
//! "Flexible" mode it reaches the origin over plain HTTP even for an
//! `https://` visitor, so a TLS-only check redirects every request back to
//! the same URL, forever.
//!
//! The forwarded scheme is only believed from a peer that is known to set it:
//!
//! - a verified Cloudflare egress address (by TCP peer, never by header),
//!   which reports the visitor's scheme in `CF-Visitor` and overwrites
//!   `X-Forwarded-Proto`;
//! - a loopback reverse proxy the operator has explicitly opted in to trust
//!   (the same switch that trusts its `X-Forwarded-For`), which must
//!   overwrite or append `X-Forwarded-Proto`.
//!
//! Every other peer — including a direct client that sends the headers itself
//! — gets the connection's own TLS state, exactly as before.
//!
//! This feeds the HTTP→HTTPS redirect decision only. What Temps forwards
//! upstream as `X-Forwarded-Proto` still reflects the local connection.

use axum::http::HeaderMap;
use std::net::IpAddr;

use crate::client_ip::peer_is_loopback;

/// Whether the end client used HTTPS.
///
/// `connection_is_tls` is the local connection's state and is the answer
/// whenever the peer is not trusted or did not say. `peer_is_cloudflare` is
/// a closure so the CIDR lookup only runs when there is a peer to check.
pub(crate) fn client_used_https(
    peer: Option<IpAddr>,
    headers: &HeaderMap,
    connection_is_tls: bool,
    trust_loopback_forwarded: bool,
    peer_is_cloudflare: impl FnOnce(IpAddr) -> bool,
) -> bool {
    let Some(peer) = peer else {
        return connection_is_tls;
    };

    let forwarded = if trust_loopback_forwarded && peer_is_loopback(peer) {
        x_forwarded_proto_is_https(headers)
    } else if peer_is_cloudflare(peer) {
        headers
            .get("cf-visitor")
            .and_then(|value| value.to_str().ok())
            .and_then(cf_visitor_is_https)
            .or_else(|| x_forwarded_proto_is_https(headers))
    } else {
        None
    };

    forwarded.unwrap_or(connection_is_tls)
}

/// Parse Cloudflare's `CF-Visitor: {"scheme":"https"}`.
///
/// Matched textually rather than with a JSON parser: the value is a fixed
/// one-key object, and this runs on every request from a Cloudflare peer.
/// Anything unrecognized is `None`, so the caller falls back.
fn cf_visitor_is_https(value: &str) -> Option<bool> {
    let compact: String = value.chars().filter(|c| !c.is_whitespace()).collect();
    if compact.contains(r#""scheme":"https""#) {
        Some(true)
    } else if compact.contains(r#""scheme":"http""#) {
        Some(false)
    } else {
        None
    }
}

/// The final entry of the final `X-Forwarded-Proto` field: a proxy that
/// appends puts its own observation last, after anything the client sent.
fn x_forwarded_proto_is_https(headers: &HeaderMap) -> Option<bool> {
    let value = headers
        .get_all("x-forwarded-proto")
        .iter()
        .next_back()?
        .to_str()
        .ok()?;
    let last = value.rsplit(',').next()?.trim();
    if last.eq_ignore_ascii_case("https") {
        Some(true)
    } else if last.eq_ignore_ascii_case("http") {
        Some(false)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{HeaderName, HeaderValue};

    const CLOUDFLARE: &str = "104.16.1.1";
    const DIRECT: &str = "203.0.113.9";

    fn headers(values: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in values {
            map.append(
                HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_str(value).unwrap(),
            );
        }
        map
    }

    fn resolve(peer: &str, values: &[(&str, &str)], tls: bool, trust_loopback: bool) -> bool {
        client_used_https(
            Some(peer.parse().unwrap()),
            &headers(values),
            tls,
            trust_loopback,
            |ip| ip == CLOUDFLARE.parse::<IpAddr>().unwrap(),
        )
    }

    #[test]
    fn cloudflare_full_mode_http_visitor_is_not_https() {
        // Cloudflare reached us over TLS, but the visitor typed http://.
        assert!(!resolve(
            CLOUDFLARE,
            &[("cf-visitor", r#"{"scheme":"http"}"#)],
            true,
            false
        ));
    }

    #[test]
    fn cloudflare_flexible_mode_https_visitor_is_https() {
        // Cloudflare reached us over plain HTTP for an https:// visitor;
        // treating this as HTTP is what loops.
        assert!(resolve(
            CLOUDFLARE,
            &[("cf-visitor", r#"{"scheme":"https"}"#)],
            false,
            false
        ));
    }

    #[test]
    fn cloudflare_visitor_tolerates_whitespace() {
        assert!(resolve(
            CLOUDFLARE,
            &[("cf-visitor", r#"{ "scheme" : "https" }"#)],
            false,
            false
        ));
    }

    #[test]
    fn cloudflare_falls_back_to_x_forwarded_proto() {
        assert!(!resolve(
            CLOUDFLARE,
            &[("x-forwarded-proto", "http")],
            true,
            false
        ));
        assert!(resolve(
            CLOUDFLARE,
            &[("x-forwarded-proto", "https")],
            false,
            false
        ));
    }

    #[test]
    fn cloudflare_visitor_wins_over_x_forwarded_proto() {
        assert!(resolve(
            CLOUDFLARE,
            &[
                ("cf-visitor", r#"{"scheme":"https"}"#),
                ("x-forwarded-proto", "http")
            ],
            false,
            false
        ));
    }

    #[test]
    fn cloudflare_without_headers_uses_connection() {
        assert!(resolve(CLOUDFLARE, &[], true, false));
        assert!(!resolve(CLOUDFLARE, &[], false, false));
    }

    #[test]
    fn garbage_values_use_connection() {
        assert!(resolve(
            CLOUDFLARE,
            &[("cf-visitor", "nonsense"), ("x-forwarded-proto", "gopher")],
            true,
            false
        ));
        assert!(!resolve(
            CLOUDFLARE,
            &[("cf-visitor", "{}"), ("x-forwarded-proto", "")],
            false,
            false
        ));
    }

    #[test]
    fn direct_client_cannot_spoof_the_scheme() {
        // A plain-HTTP client claiming https must still be redirected...
        assert!(!resolve(
            DIRECT,
            &[
                ("cf-visitor", r#"{"scheme":"https"}"#),
                ("x-forwarded-proto", "https")
            ],
            false,
            true
        ));
        // ...and a TLS client claiming http must not be bounced.
        assert!(resolve(
            DIRECT,
            &[
                ("cf-visitor", r#"{"scheme":"http"}"#),
                ("x-forwarded-proto", "http")
            ],
            true,
            true
        ));
    }

    #[test]
    fn loopback_proxy_is_trusted_only_when_opted_in() {
        let xfp = [("x-forwarded-proto", "https")];
        assert!(resolve("127.0.0.1", &xfp, false, true));
        assert!(resolve("::1", &xfp, false, true));
        assert!(resolve("::ffff:127.0.0.1", &xfp, false, true));
        assert!(!resolve("127.0.0.1", &xfp, false, false));
    }

    #[test]
    fn loopback_proxy_ignores_cf_visitor() {
        // Only the header the local proxy is required to set counts.
        assert!(!resolve(
            "127.0.0.1",
            &[("cf-visitor", r#"{"scheme":"https"}"#)],
            false,
            true
        ));
    }

    #[test]
    fn appended_x_forwarded_proto_uses_last_entry() {
        assert!(!resolve(
            "127.0.0.1",
            &[("x-forwarded-proto", "https, http")],
            true,
            true
        ));
        assert!(resolve(
            "127.0.0.1",
            &[
                ("x-forwarded-proto", "http"),
                ("x-forwarded-proto", "HTTPS")
            ],
            false,
            true
        ));
    }

    #[test]
    fn no_inet_peer_uses_connection() {
        let map = headers(&[("x-forwarded-proto", "https")]);
        assert!(!client_used_https(None, &map, false, true, |_| true));
        assert!(client_used_https(None, &map, true, true, |_| true));
    }
}
