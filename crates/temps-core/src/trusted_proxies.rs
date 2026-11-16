// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Immutable inbound proxy trust, published by the settings service outside traffic.

use arc_swap::ArcSwap;
use axum::http::HeaderMap;
use ipnet::IpNet;
use std::net::IpAddr;
use std::sync::{Arc, LazyLock};

pub const MAX_TRUSTED_PROXY_RANGES: usize = 64;

#[derive(Debug, thiserror::Error)]
pub enum TrustedProxyError {
    #[error(
        "Cannot configure {count} trusted proxy ranges; the maximum is {MAX_TRUSTED_PROXY_RANGES}"
    )]
    TooMany { count: usize },
    #[error("Invalid trusted proxy range '{entry}': {reason}")]
    InvalidRange { entry: String, reason: String },
}

#[derive(Debug, Default)]
pub struct TrustedProxyRanges {
    ranges: Vec<IpNet>,
}

fn canonical_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(ip) => ip
            .to_ipv4_mapped()
            .map(IpAddr::V4)
            .unwrap_or(IpAddr::V6(ip)),
        other => other,
    }
}

impl TrustedProxyRanges {
    pub fn parse(entries: &[String]) -> Result<Self, TrustedProxyError> {
        if entries.len() > MAX_TRUSTED_PROXY_RANGES {
            return Err(TrustedProxyError::TooMany {
                count: entries.len(),
            });
        }
        let mut ranges = Vec::with_capacity(entries.len());
        for entry in entries {
            let invalid = |reason: &str| TrustedProxyError::InvalidRange {
                entry: entry.chars().take(64).collect(),
                reason: reason.to_string(),
            };
            if entry.len() > 64 {
                return Err(invalid("an IP address or CIDR must be at most 64 bytes"));
            }
            let entry = entry.trim();
            let range = match entry.parse::<IpAddr>() {
                Ok(ip) => IpNet::from(canonical_ip(ip)),
                Err(_) => entry
                    .parse::<IpNet>()
                    .map_err(|_| invalid("expected an IP address or IPv4/IPv6 CIDR"))?
                    .trunc(),
            };
            // Peers are canonicalized to IPv4, so mapped networks must use
            // the same family. A mapped /96 would trust every IPv4 peer.
            let range = match range {
                IpNet::V6(net) if net.prefix_len() >= 96 => match net.addr().to_ipv4_mapped() {
                    Some(ip) => IpNet::new(IpAddr::V4(ip), net.prefix_len() - 96)
                        .map_err(|_| invalid("invalid IPv4-mapped prefix"))?
                        .trunc(),
                    None => range,
                },
                _ => range,
            };
            if range.prefix_len() == 0 {
                return Err(invalid("trusting every address would permit client IP spoofing; specify the proxy ranges"));
            }
            if !ranges.contains(&range) {
                ranges.push(range);
            }
        }
        Ok(Self { ranges })
    }

    pub fn canonical_ranges(&self) -> Vec<String> {
        self.ranges.iter().map(ToString::to_string).collect()
    }

    /// Whether the direct socket peer is explicitly trusted, without reading
    /// forwarded headers or traversing the client-address chain.
    pub fn trusts_peer(&self, peer: IpAddr) -> bool {
        self.trusts(peer, false)
    }

    fn trusts(&self, ip: IpAddr, loopback: bool) -> bool {
        let ip = canonical_ip(ip);
        (loopback && ip.is_loopback()) || self.ranges.iter().any(|range| range.contains(&ip))
    }

    /// Walk the final header field and its hops right-to-left, stopping at the
    /// first untrusted address. A malformed traversed hop fails closed to the peer.
    /// At most 64 CIDRs are scanned per hop; no locks, I/O or per-hop allocation.
    pub fn resolve(&self, peer: IpAddr, headers: &HeaderMap, loopback: bool) -> Option<IpAddr> {
        if !self.trusts(peer, loopback) {
            return None;
        }
        let mut address = peer;
        let values = headers.get_all("x-forwarded-for");
        if values.iter().next().is_some() {
            for value in values.iter().rev() {
                if !self.trusts(address, loopback) {
                    return Some(canonical_ip(address));
                }
                let Ok(value) = value.to_str() else {
                    return Some(peer);
                };
                for hop in value.rsplit(',') {
                    if !self.trusts(address, loopback) {
                        return Some(canonical_ip(address));
                    }
                    let Ok(ip) = hop.trim().parse::<IpAddr>() else {
                        return Some(peer);
                    };
                    address = ip;
                }
            }
            Some(canonical_ip(address))
        } else {
            Some(
                headers
                    .get("x-real-ip")
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.trim().parse::<IpAddr>().ok())
                    .map(canonical_ip)
                    .unwrap_or(peer),
            )
        }
    }
}

static PROXY_RANGES: LazyLock<ArcSwap<TrustedProxyRanges>> =
    LazyLock::new(|| ArcSwap::from_pointee(TrustedProxyRanges::default()));

pub fn set_trusted_proxy_ranges(ranges: TrustedProxyRanges) {
    PROXY_RANGES.store(Arc::new(ranges));
}

pub fn is_trusted_proxy_peer(peer: IpAddr) -> bool {
    PROXY_RANGES.load().trusts_peer(peer)
}

pub fn resolve_trusted_proxy_ip(
    peer: IpAddr,
    headers: &HeaderMap,
    loopback: bool,
) -> Option<IpAddr> {
    PROXY_RANGES.load().resolve(peer, headers, loopback)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolve(peer: &str, fields: &[(&str, &str)]) -> Option<IpAddr> {
        let ranges =
            TrustedProxyRanges::parse(&["10.20.0.0/16".into(), "2001:db8:1::/48".into()]).unwrap();
        let mut headers = HeaderMap::new();
        for (name, value) in fields {
            headers.append(
                axum::http::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                value.parse().unwrap(),
            );
        }
        ranges.resolve(peer.parse().unwrap(), &headers, false)
    }

    #[test]
    fn walks_trusted_hops_and_ignores_forged_prefix() {
        assert_eq!(
            resolve(
                "10.20.0.2",
                &[("x-forwarded-for", "192.0.2.99, 198.51.100.7, 10.20.0.1")]
            ),
            Some("198.51.100.7".parse().unwrap())
        );
        assert_eq!(
            resolve(
                "10.20.0.2",
                &[
                    ("x-forwarded-for", "192.0.2.99"),
                    ("x-forwarded-for", "198.51.100.7, 10.20.0.1")
                ]
            ),
            Some("198.51.100.7".parse().unwrap())
        );
        assert_eq!(
            resolve("198.51.100.7", &[("x-forwarded-for", "192.0.2.99")]),
            None
        );
    }

    #[test]
    fn ipv6_and_mapped_peers_work() {
        assert_eq!(
            resolve("2001:db8:1::2", &[("x-forwarded-for", "2001:db8:2::7")]),
            Some("2001:db8:2::7".parse().unwrap())
        );
        assert_eq!(
            resolve("::ffff:10.20.0.2", &[("x-forwarded-for", "198.51.100.7")]),
            Some("198.51.100.7".parse().unwrap())
        );
    }

    #[test]
    fn peer_trust_needs_no_forwarded_header_chain() {
        let ranges = TrustedProxyRanges::parse(&["10.20.0.0/16".into()]).unwrap();
        assert!(ranges.trusts_peer("10.20.0.2".parse().unwrap()));
        assert!(ranges.trusts_peer("::ffff:10.20.0.2".parse().unwrap()));
        assert!(!ranges.trusts_peer("198.51.100.7".parse().unwrap()));
        assert!(!ranges.trusts_peer("127.0.0.1".parse().unwrap()));
    }

    #[test]
    fn mapped_cidrs_match_canonical_peers() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "198.51.100.7".parse().unwrap());
        for (entry, canonical) in [
            ("::ffff:10.20.0.0/112", "10.20.0.0/16"),
            ("::ffff:10.20.0.2/128", "10.20.0.2/32"),
        ] {
            let ranges = TrustedProxyRanges::parse(&[entry.into()]).unwrap();
            assert_eq!(ranges.canonical_ranges(), [canonical]);
            for peer in ["10.20.0.2", "::ffff:10.20.0.2"] {
                assert_eq!(
                    ranges.resolve(peer.parse().unwrap(), &headers, false),
                    Some("198.51.100.7".parse().unwrap())
                );
            }
            assert_eq!(
                ranges.resolve("10.21.0.2".parse().unwrap(), &headers, false),
                None
            );
        }
        assert!(TrustedProxyRanges::parse(&["::ffff:0.0.0.0/96".into()]).is_err());
    }

    #[test]
    fn malformed_hops_fail_closed_without_real_ip_fallback() {
        for value in [
            "",
            "198.51.100.7, garbage",
            "198.51.100.7,",
            "198.51.100.7:443",
        ] {
            assert_eq!(
                resolve(
                    "10.20.0.2",
                    &[("x-forwarded-for", value), ("x-real-ip", "192.0.2.99")]
                ),
                Some("10.20.0.2".parse().unwrap())
            );
        }
        assert_eq!(
            resolve("10.20.0.2", &[("x-real-ip", "198.51.100.7")]),
            Some("198.51.100.7".parse().unwrap())
        );
        assert_eq!(
            resolve("10.20.0.2", &[]),
            Some("10.20.0.2".parse().unwrap())
        );
    }

    #[test]
    fn untrusted_prefix_fields_are_not_parsed() {
        let ranges = TrustedProxyRanges::parse(&["10.20.0.0/16".into()]).unwrap();
        let mut headers = HeaderMap::new();
        headers.append(
            "x-forwarded-for",
            axum::http::HeaderValue::from_bytes(&[0xff]).unwrap(),
        );
        headers.append("x-forwarded-for", "198.51.100.7".parse().unwrap());
        assert_eq!(
            ranges.resolve("10.20.0.2".parse().unwrap(), &headers, false),
            Some("198.51.100.7".parse().unwrap())
        );
        headers.remove("x-forwarded-for");
        headers.append(
            "x-forwarded-for",
            axum::http::HeaderValue::from_bytes(&[0xff]).unwrap(),
        );
        assert_eq!(
            ranges.resolve("10.20.0.2".parse().unwrap(), &headers, false),
            Some("10.20.0.2".parse().unwrap())
        );
    }

    #[test]
    fn validates_bounds_and_normalizes_networks() {
        for entry in ["", "invalid", "0.0.0.0/0", "::/0", "10.0.0.1/33"] {
            assert!(TrustedProxyRanges::parse(&[entry.into()]).is_err());
        }
        assert!(TrustedProxyRanges::parse(&vec!["10.0.0.1".into(); 65]).is_err());
        assert_eq!(
            TrustedProxyRanges::parse(&[
                "10.20.1.2/16".into(),
                "10.20.0.0/16".into(),
                "198.51.100.7".into()
            ])
            .unwrap()
            .canonical_ranges(),
            ["10.20.0.0/16", "198.51.100.7/32"]
        );
    }
}
