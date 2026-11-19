// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Address ranges of reverse proxies and CDNs that front origins, so an
//! address in one is never mistaken for the origin itself.
//!
//! Resolving a hostname that sits behind such a proxy returns the proxy's
//! edge, not the server behind it. An `A` record pointing at that edge with
//! the proxy turned off sends visitors to an edge that has no idea which
//! origin to forward to: the address looks public and valid, but nothing
//! there serves the app. Anything that picks an address to point DNS at
//! checks it against [`known_proxy`] first.
//!
//! The ranges are vendored from the providers' published lists. They change
//! on the order of years and are announced in advance; the proxy's
//! `CF-Connecting-IP` trust store (`temps-proxy`) starts from the same
//! Cloudflare list and refreshes it at runtime.

use std::net::IpAddr;

use ipnet::IpNet;

/// A reverse proxy or CDN whose published ranges Temps knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KnownProxy {
    Cloudflare,
}

impl KnownProxy {
    /// The provider's name, for messages.
    pub fn name(self) -> &'static str {
        match self {
            KnownProxy::Cloudflare => "Cloudflare",
        }
    }
}

impl std::fmt::Display for KnownProxy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// Cloudflare's published edge ranges (<https://www.cloudflare.com/ips/>),
/// vendored 2026-07-16.
pub const CLOUDFLARE_RANGES: &[&str] = &[
    // IPv4
    "173.245.48.0/20",
    "103.21.244.0/22",
    "103.22.200.0/22",
    "103.31.4.0/22",
    "141.101.64.0/18",
    "108.162.192.0/18",
    "190.93.240.0/20",
    "188.114.96.0/20",
    "197.234.240.0/22",
    "198.41.128.0/17",
    "162.158.0.0/15",
    "104.16.0.0/13",
    "104.24.0.0/14",
    "172.64.0.0/13",
    "131.0.72.0/22",
    // IPv6
    "2400:cb00::/32",
    "2606:4700::/32",
    "2803:f800::/32",
    "2405:b500::/32",
    "2405:8100::/32",
    "2a06:98c0::/29",
    "2c0f:f248::/32",
];

/// Every known provider with its ranges.
const KNOWN: &[(KnownProxy, &[&str])] = &[(KnownProxy::Cloudflare, CLOUDFLARE_RANGES)];

/// The proxy or CDN whose edge `ip` belongs to, if Temps knows it. An
/// IPv4-mapped IPv6 address is judged as the IPv4 address it carries.
pub fn known_proxy(ip: IpAddr) -> Option<KnownProxy> {
    let ip = match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(IpAddr::V6(v6), IpAddr::V4),
        v4 => v4,
    };
    KNOWN.iter().find_map(|(proxy, ranges)| {
        ranges
            .iter()
            .filter_map(|cidr| cidr.parse::<IpNet>().ok())
            .any(|net| net.contains(&ip))
            .then_some(*proxy)
    })
}

/// Why `ip` cannot be an origin address: what pointing a DNS-only record at
/// it would do, and what to use instead. `None` when it is no known
/// proxy's edge.
pub fn origin_refusal(ip: IpAddr) -> Option<String> {
    let proxy = known_proxy(ip)?;
    Some(format!(
        "{ip} belongs to {proxy}'s edge, not to this server: a hostname behind {proxy}'s proxy \
         resolves to {proxy}, so it says nothing about the server's own address. A DNS-only \
         record pointing at {ip} would send visitors to {proxy} with nothing behind it."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_vendored_range_parses() {
        for (_, ranges) in KNOWN {
            for cidr in *ranges {
                assert!(cidr.parse::<IpNet>().is_ok(), "{cidr}");
            }
        }
    }

    #[test]
    fn cloudflare_edges_are_recognised() {
        for ip in [
            "104.16.0.1",
            "172.67.1.1",
            "162.159.200.1",
            "188.114.97.3",
            "2606:4700:3030::6815:1",
            "::ffff:104.21.3.4",
        ] {
            assert_eq!(
                known_proxy(ip.parse().unwrap()),
                Some(KnownProxy::Cloudflare),
                "{ip}"
            );
        }
    }

    #[test]
    fn ordinary_addresses_are_not_proxies() {
        for ip in [
            "192.0.2.10",
            "198.51.100.7",
            "203.0.113.9",
            "10.0.0.5",
            "104.15.255.255",
            "2001:db8::1",
        ] {
            assert_eq!(known_proxy(ip.parse().unwrap()), None, "{ip}");
            assert_eq!(origin_refusal(ip.parse().unwrap()), None, "{ip}");
        }
    }

    #[test]
    fn the_refusal_names_the_address_and_the_provider() {
        let message = origin_refusal("104.16.0.1".parse().unwrap()).unwrap();
        assert!(message.contains("104.16.0.1"), "{message}");
        assert!(message.contains("Cloudflare"), "{message}");
    }
}
