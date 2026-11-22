// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! IP networks as integers, so lists sort, nest and search without
//! allocating.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// A CIDR network with its host bits cleared. IPv4-mapped IPv6 networks are
/// stored as IPv4, so a list and a lookup agree on which family an address
/// is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Network {
    V4 { start: u32, prefix: u8 },
    V6 { start: u128, prefix: u8 },
}

impl Network {
    /// `a.b.c.d/n`, `x::/n`, or a bare address (a single host). Host bits
    /// are cleared, so `10.1.2.3/8` reads as `10.0.0.0/8`.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let (address, prefix) = match text.split_once('/') {
            Some((address, prefix)) => (address, Some(prefix.parse::<u8>().ok()?)),
            None => (text, None),
        };
        match address.parse::<IpAddr>().ok()? {
            IpAddr::V4(v4) => Self::v4(u32::from(v4), prefix.unwrap_or(32)),
            IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
                Some(v4) => {
                    let prefix = prefix.unwrap_or(128).checked_sub(96)?;
                    Self::v4(u32::from(v4), prefix)
                }
                None => Self::v6(u128::from(v6), prefix.unwrap_or(128)),
            },
        }
    }

    pub fn v4(address: u32, prefix: u8) -> Option<Self> {
        (prefix <= 32).then(|| Self::V4 {
            start: address & mask4(prefix),
            prefix,
        })
    }

    pub fn v6(address: u128, prefix: u8) -> Option<Self> {
        (prefix <= 128).then(|| Self::V6 {
            start: address & mask6(prefix),
            prefix,
        })
    }

    pub fn prefix(&self) -> u8 {
        match *self {
            Self::V4 { prefix, .. } | Self::V6 { prefix, .. } => prefix,
        }
    }

    pub fn contains(&self, ip: IpAddr) -> bool {
        match (*self, Address::from(ip)) {
            (Self::V4 { start, prefix }, Address::V4(ip)) => ip & mask4(prefix) == start,
            (Self::V6 { start, prefix }, Address::V6(ip)) => ip & mask6(prefix) == start,
            _ => false,
        }
    }
}

impl fmt::Display for Network {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::V4 { start, prefix } => write!(f, "{}/{prefix}", Ipv4Addr::from(start)),
            Self::V6 { start, prefix } => write!(f, "{}/{prefix}", Ipv6Addr::from(start)),
        }
    }
}

/// An address as the integer the lists are keyed by. IPv4-mapped IPv6
/// addresses are IPv4.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Address {
    V4(u32),
    V6(u128),
}

impl From<IpAddr> for Address {
    fn from(ip: IpAddr) -> Self {
        match ip.to_canonical() {
            IpAddr::V4(v4) => Self::V4(u32::from(v4)),
            IpAddr::V6(v6) => Self::V6(u128::from(v6)),
        }
    }
}

pub(crate) fn mask4(prefix: u8) -> u32 {
    u32::MAX
        .checked_shl(32u32.saturating_sub(u32::from(prefix)))
        .unwrap_or(0)
}

pub(crate) fn mask6(prefix: u8) -> u128 {
    u128::MAX
        .checked_shl(128u32.saturating_sub(u32::from(prefix)))
        .unwrap_or(0)
}

/// Sorts `networks` and drops every one inside another. CIDR networks are
/// either nested or disjoint, so what is left is disjoint, and the last one
/// starting at or before an address is the only one that can hold it.
pub(crate) fn collapse<T: Copy + Ord>(
    networks: &mut Vec<(T, u8)>,
    contains: impl Fn((T, u8), T) -> bool,
) {
    networks.sort_unstable_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    let mut kept: Vec<(T, u8)> = Vec::with_capacity(networks.len());
    for &network in networks.iter() {
        if kept.last().is_some_and(|&last| contains(last, network.0)) {
            continue;
        }
        kept.push(network);
    }
    *networks = kept;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(text: &str) -> IpAddr {
        text.parse().unwrap()
    }

    #[test]
    fn parses_networks_and_clears_host_bits() {
        assert_eq!(
            Network::parse("10.1.2.3/8").unwrap().to_string(),
            "10.0.0.0/8"
        );
        assert_eq!(
            Network::parse("203.0.113.7").unwrap().to_string(),
            "203.0.113.7/32"
        );
        assert_eq!(
            Network::parse("2001:db8:1::5/32").unwrap().to_string(),
            "2001:db8::/32"
        );
        assert_eq!(
            Network::parse("0.0.0.0/0").unwrap().to_string(),
            "0.0.0.0/0"
        );
        assert_eq!(Network::parse("::/0").unwrap().to_string(), "::/0");
        assert_eq!(
            Network::parse("::ffff:203.0.113.0/120")
                .unwrap()
                .to_string(),
            "203.0.113.0/24"
        );
        for bad in [
            "",
            "10.0.0.0/33",
            "::/129",
            "nope",
            "10.0.0.0/x",
            "::ffff:1.2.3.4/90",
        ] {
            assert_eq!(Network::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn containment_follows_the_prefix_and_maps_ipv4_in_ipv6() {
        let net = Network::parse("203.0.113.0/24").unwrap();
        assert!(net.contains(ip("203.0.113.200")));
        assert!(net.contains(ip("::ffff:203.0.113.9")));
        assert!(!net.contains(ip("203.0.114.1")));
        assert!(!net.contains(ip("2001:db8::1")));
        assert!(Network::parse("0.0.0.0/0").unwrap().contains(ip("8.8.8.8")));
    }

    #[test]
    fn collapsing_drops_nested_networks() {
        let mut nets = vec![
            (u32::from(Ipv4Addr::new(10, 1, 0, 0)), 16),
            (u32::from(Ipv4Addr::new(10, 0, 0, 0)), 8),
            (u32::from(Ipv4Addr::new(11, 0, 0, 0)), 8),
            (u32::from(Ipv4Addr::new(10, 0, 0, 0)), 8),
        ];
        collapse(&mut nets, |(start, prefix), x| x & mask4(prefix) == start);
        assert_eq!(
            nets,
            vec![
                (u32::from(Ipv4Addr::new(10, 0, 0, 0)), 8),
                (u32::from(Ipv4Addr::new(11, 0, 0, 0)), 8),
            ]
        );
    }
}
