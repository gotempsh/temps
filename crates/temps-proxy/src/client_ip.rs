// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Forwarded client addresses from a reverse proxy on the same host.
//!
//! The local proxy must overwrite X-Forwarded-For with the connection address,
//! or append that address after any client-supplied entries. Remote peers never
//! gain trust by supplying headers. This matches the console's loopback trust
//! boundary without treating private networks or arbitrary CDN headers as trusted.

use axum::http::HeaderMap;
use std::net::IpAddr;

/// Return a trusted loopback peer's forwarded address, or its own address when
/// the header is missing/invalid. `None` leaves non-loopback peers to the
/// existing CDN/direct-connection resolution path.
pub(crate) fn resolve_loopback_client_ip(peer: IpAddr, headers: &HeaderMap) -> Option<IpAddr> {
    if !peer.is_loopback() {
        return None;
    }

    // Use the final entry in the final field: an earlier field/list entry can
    // have been supplied by the client before the local proxy appended its IP.
    // A malformed XFF fails closed to the peer, not to an earlier entry or XRI.
    let forwarded = if let Some(value) = headers.get_all("x-forwarded-for").iter().next_back() {
        value
            .to_str()
            .ok()
            .and_then(|value| value.rsplit(',').next())
            .and_then(|value| value.trim().parse::<IpAddr>().ok())
    } else {
        headers
            .get("x-real-ip")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.trim().parse::<IpAddr>().ok())
    };

    Some(forwarded.unwrap_or(peer))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolve(peer: &str, values: &[(&str, &str)]) -> Option<IpAddr> {
        let mut headers = HeaderMap::new();
        for (name, value) in values {
            headers.append(
                axum::http::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                value.parse().unwrap(),
            );
        }
        resolve_loopback_client_ip(peer.parse().unwrap(), &headers)
    }

    #[test]
    fn loopback_forwards_ipv4_and_ipv6_clients() {
        for peer in ["127.0.0.1", "127.0.0.2", "::1"] {
            for client in ["198.51.100.23", "2001:db8::23"] {
                assert_eq!(
                    resolve(peer, &[("x-forwarded-for", client)]),
                    Some(client.parse().unwrap())
                );
            }
        }
    }

    #[test]
    fn uses_rightmost_entry_in_last_forwarded_field() {
        assert_eq!(
            resolve(
                "127.0.0.1",
                &[
                    ("x-forwarded-for", "192.0.2.1"),
                    ("x-forwarded-for", "192.0.2.2,  2001:db8::23  "),
                    ("x-real-ip", "192.0.2.3"),
                ],
            ),
            Some("2001:db8::23".parse().unwrap())
        );
    }

    #[test]
    fn falls_back_to_real_ip_only_when_forwarded_for_is_absent() {
        assert_eq!(
            resolve("::1", &[("x-real-ip", " 198.51.100.23 ")]),
            Some("198.51.100.23".parse().unwrap())
        );
        assert_eq!(
            resolve("::1", &[("x-real-ip", "2001:db8::23")]),
            Some("2001:db8::23".parse().unwrap())
        );
    }

    #[test]
    fn rejects_malformed_forwarded_values_without_trusting_other_headers() {
        for value in [
            "",
            "unknown",
            "198.51.100.23:443",
            "[2001:db8::23]:443",
            "198.51.100.23, ",
            "198.51.100.23, garbage",
        ] {
            assert_eq!(
                resolve(
                    "127.0.0.1",
                    &[("x-forwarded-for", value), ("x-real-ip", "192.0.2.3")],
                ),
                Some("127.0.0.1".parse().unwrap()),
                "invalid forwarded value: {value:?}",
            );
        }
    }

    #[test]
    fn missing_or_invalid_real_ip_keeps_the_loopback_peer() {
        assert_eq!(resolve("::1", &[]), Some("::1".parse().unwrap()));
        for value in ["", "unknown", "192.0.2.1, 192.0.2.2", "192.0.2.1:443"] {
            assert_eq!(
                resolve("::1", &[("x-real-ip", value)]),
                Some("::1".parse().unwrap())
            );
        }
    }

    #[test]
    fn remote_and_private_peers_cannot_spoof_forwarded_addresses() {
        for peer in [
            "203.0.113.10",
            "10.0.0.2",
            "192.168.1.2",
            "2001:db8::10",
            "fd00::2",
        ] {
            assert_eq!(
                resolve(
                    peer,
                    &[
                        ("x-forwarded-for", "198.51.100.23"),
                        ("x-real-ip", "198.51.100.24"),
                        ("cf-connecting-ip", "198.51.100.25"),
                    ],
                ),
                None,
                "untrusted peer: {peer}",
            );
        }
    }

    #[test]
    fn loopback_does_not_trust_a_client_supplied_cdn_header() {
        assert_eq!(
            resolve("127.0.0.1", &[("cf-connecting-ip", "198.51.100.23")]),
            Some("127.0.0.1".parse().unwrap())
        );
    }

    #[test]
    fn rejects_non_text_forwarded_header() {
        let peer = "127.0.0.1".parse().unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            axum::http::HeaderValue::from_bytes(&[0xff]).unwrap(),
        );
        assert_eq!(resolve_loopback_client_ip(peer, &headers), Some(peer));
    }
}
