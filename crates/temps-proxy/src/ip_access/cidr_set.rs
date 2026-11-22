// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! CIDR parsing, validation and matching for per-project IP access lists.
//!
//! Parsing is strict and happens on write; matching happens per request, so
//! [`CidrSet`] is a pair of sorted, merged integer-range vectors (one per
//! address family) searched with `partition_point`: O(log n), no allocation,
//! no locks.
//!
//! IPv4-mapped IPv6 (`::ffff:a.b.c.d`) is folded into IPv4 on both sides:
//! a rule written as `::ffff:10.0.0.0/104` becomes `10.0.0.0/8`, and a client
//! address in mapped form is matched as the IPv4 address it carries. A dual
//! stack listener hands same-family IPv4 clients to Rust in mapped form, so
//! without this a `10.0.0.0/8` rule would silently never match.
//!
//! Note the converse: `::/0` matches IPv6 clients only. To cover everything,
//! list both `0.0.0.0/0` and `::/0`.

use ipnetwork::IpNetwork;
use std::net::IpAddr;
use thiserror::Error;

/// Upper bound on entries in one policy. Keeps the snapshot (and the
/// admin-facing editor) bounded; 256 is far above any hand-maintained list.
pub const MAX_CIDRS_PER_POLICY: usize = 256;

/// Upper bound on a free-text entry description.
pub const MAX_DESCRIPTION_LEN: usize = 120;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CidrError {
    #[error("'{input}' is not a valid IPv4/IPv6 address or CIDR: {reason}")]
    Invalid { input: String, reason: String },

    #[error(
        "'{input}' has host bits set below its /{prefix} prefix; use the network address \
         '{suggested}' (write a single host as /32 for IPv4 or /128 for IPv6)"
    )]
    HostBitsSet {
        input: String,
        prefix: u8,
        suggested: String,
    },
}

/// Parse one CIDR strictly and return it in canonical form.
///
/// * A bare address (`203.0.113.7`) is a single host (`/32` or `/128`).
/// * Host bits set (`10.0.0.5/8`) is rejected, not silently masked: the user
///   usually meant something narrower, and widening an allow-list without
///   saying so is exactly the wrong failure.
/// * An IPv4-mapped IPv6 network with prefix >= 96 is returned as the
///   equivalent IPv4 network.
pub fn parse_cidr(input: &str) -> Result<IpNetwork, CidrError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(CidrError::Invalid {
            input: input.to_string(),
            reason: "value is empty".to_string(),
        });
    }
    if trimmed.len() > 64 {
        return Err(CidrError::Invalid {
            input: truncate_for_message(trimmed),
            reason: "value is too long".to_string(),
        });
    }

    let network: IpNetwork =
        trimmed
            .parse()
            .map_err(|e: ipnetwork::IpNetworkError| CidrError::Invalid {
                input: trimmed.to_string(),
                reason: e.to_string(),
            })?;

    if network.ip() != network.network() {
        return Err(CidrError::HostBitsSet {
            input: trimmed.to_string(),
            prefix: network.prefix(),
            suggested: format!("{}/{}", network.network(), network.prefix()),
        });
    }

    if let IpNetwork::V6(v6) = network {
        if v6.prefix() >= 96 {
            if let Some(v4) = v6.network().to_ipv4_mapped() {
                return IpNetwork::new(IpAddr::V4(v4), v6.prefix() - 96).map_err(|e| {
                    CidrError::Invalid {
                        input: trimmed.to_string(),
                        reason: e.to_string(),
                    }
                });
            }
        }
    }

    Ok(network)
}

fn truncate_for_message(value: &str) -> String {
    value.chars().take(64).collect()
}

/// Inclusive `[start, end]` ranges, sorted and non-overlapping.
type Ranges<T> = Vec<(T, T)>;

/// An immutable set of IPv4/IPv6 networks answering "does this address fall
/// inside any of them?" in O(log n) without allocating.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CidrSet {
    v4: Ranges<u32>,
    v6: Ranges<u128>,
}

impl CidrSet {
    /// Build a set from already-parsed networks. Overlapping and adjacent
    /// ranges are merged, so lookup cost depends on distinct coverage, not on
    /// how many redundant entries were listed.
    pub fn from_networks<'a>(networks: impl IntoIterator<Item = &'a IpNetwork>) -> Self {
        let mut v4: Ranges<u32> = Vec::new();
        let mut v6: Ranges<u128> = Vec::new();
        for network in networks {
            match network {
                IpNetwork::V4(n) => {
                    let start = u32::from(n.network());
                    let host_mask = u32::MAX.checked_shr(u32::from(n.prefix())).unwrap_or(0);
                    v4.push((start, start | host_mask));
                }
                IpNetwork::V6(n) => {
                    let start = u128::from(n.network());
                    let host_mask = u128::MAX.checked_shr(u32::from(n.prefix())).unwrap_or(0);
                    v6.push((start, start | host_mask));
                }
            }
        }
        Self {
            v4: merge(v4),
            v6: merge(v6),
        }
    }

    /// Parse and build in one step; fails on the first invalid entry.
    pub fn parse<'a>(entries: impl IntoIterator<Item = &'a str>) -> Result<Self, CidrError> {
        let networks = entries
            .into_iter()
            .map(parse_cidr)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self::from_networks(&networks))
    }

    pub fn is_empty(&self) -> bool {
        self.v4.is_empty() && self.v6.is_empty()
    }

    /// Whether `ip` is inside any network in the set. IPv4-mapped IPv6
    /// addresses are matched as the IPv4 address they carry.
    pub fn contains(&self, ip: IpAddr) -> bool {
        match ip {
            IpAddr::V4(v4) => contains_range(&self.v4, u32::from(v4)),
            IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
                Some(v4) => contains_range(&self.v4, u32::from(v4)),
                None => contains_range(&self.v6, u128::from(v6)),
            },
        }
    }
}

fn merge<T>(mut ranges: Ranges<T>) -> Ranges<T>
where
    T: Copy + Ord + MaxValue + std::ops::Add<Output = T> + From<u8>,
{
    ranges.sort_unstable();
    let mut merged: Ranges<T> = Vec::with_capacity(ranges.len());
    for (start, end) in ranges {
        match merged.last_mut() {
            // Overlapping or directly adjacent: extend. The explicit MAX check
            // avoids overflowing `end + 1` for a range ending at the top.
            Some(last) if last.1 == T::MAX_VALUE || start <= last.1 + T::from(1u8) => {
                if end > last.1 {
                    last.1 = end;
                }
            }
            _ => merged.push((start, end)),
        }
    }
    merged.shrink_to_fit();
    merged
}

fn contains_range<T: Copy + Ord>(ranges: &[(T, T)], value: T) -> bool {
    // First range whose start is > value; the candidate is the one before it.
    let idx = ranges.partition_point(|range| range.0 <= value);
    idx > 0 && ranges[idx - 1].1 >= value
}

trait MaxValue {
    const MAX_VALUE: Self;
}
impl MaxValue for u32 {
    const MAX_VALUE: u32 = u32::MAX;
}
impl MaxValue for u128 {
    const MAX_VALUE: u128 = u128::MAX;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(entries: &[&str]) -> CidrSet {
        CidrSet::parse(entries.iter().copied()).expect("test CIDRs are valid")
    }

    fn ip(value: &str) -> IpAddr {
        value.parse().expect("test IP is valid")
    }

    #[test]
    fn empty_set_matches_nothing() {
        let s = CidrSet::default();
        assert!(s.is_empty());
        assert!(!s.contains(ip("10.0.0.1")));
        assert!(!s.contains(ip("::1")));
    }

    #[test]
    fn v4_prefix_boundaries() {
        let s = set(&["100.64.0.0/10"]);
        assert!(s.contains(ip("100.64.0.0")));
        assert!(s.contains(ip("100.127.255.255")));
        assert!(!s.contains(ip("100.63.255.255")));
        assert!(!s.contains(ip("100.128.0.0")));
    }

    #[test]
    fn v4_single_host_slash_32() {
        let s = set(&["203.0.113.7/32"]);
        assert!(s.contains(ip("203.0.113.7")));
        assert!(!s.contains(ip("203.0.113.6")));
        assert!(!s.contains(ip("203.0.113.8")));
    }

    #[test]
    fn bare_address_is_a_single_host() {
        let s = set(&["203.0.113.7", "2001:db8::7"]);
        assert!(s.contains(ip("203.0.113.7")));
        assert!(!s.contains(ip("203.0.113.8")));
        assert!(s.contains(ip("2001:db8::7")));
        assert!(!s.contains(ip("2001:db8::8")));
    }

    #[test]
    fn v4_slash_zero_matches_every_v4_but_no_v6() {
        let s = set(&["0.0.0.0/0"]);
        assert!(s.contains(ip("0.0.0.0")));
        assert!(s.contains(ip("255.255.255.255")));
        assert!(s.contains(ip("8.8.8.8")));
        assert!(!s.contains(ip("2001:db8::1")));
    }

    #[test]
    fn v6_slash_zero_matches_every_v6_but_no_v4() {
        let s = set(&["::/0"]);
        assert!(s.contains(ip("::")));
        assert!(s.contains(ip("ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff")));
        assert!(s.contains(ip("2001:db8::1")));
        assert!(!s.contains(ip("8.8.8.8")));
    }

    #[test]
    fn v6_prefix_boundaries_and_slash_128() {
        let s = set(&["fd7a:115c:a1e0::/48", "2001:db8::1/128"]);
        assert!(s.contains(ip("fd7a:115c:a1e0::")));
        assert!(s.contains(ip("fd7a:115c:a1e0:ffff:ffff:ffff:ffff:ffff")));
        assert!(!s.contains(ip("fd7a:115c:a1e1::")));
        assert!(!s.contains(ip("fd7a:115c:a1df:ffff:ffff:ffff:ffff:ffff")));
        assert!(s.contains(ip("2001:db8::1")));
        assert!(!s.contains(ip("2001:db8::2")));
    }

    #[test]
    fn v4_mapped_client_matches_a_v4_rule() {
        let s = set(&["10.0.0.0/8"]);
        assert!(s.contains(ip("::ffff:10.1.2.3")));
        assert!(!s.contains(ip("::ffff:11.0.0.1")));
    }

    #[test]
    fn v4_mapped_rule_is_folded_into_v4() {
        let s = set(&["::ffff:10.0.0.0/104"]);
        assert!(s.contains(ip("10.9.9.9")));
        assert!(s.contains(ip("::ffff:10.9.9.9")));
        assert!(!s.contains(ip("11.0.0.1")));
        assert_eq!(
            parse_cidr("::ffff:10.0.0.0/104").map(|n| n.to_string()),
            Ok("10.0.0.0/8".to_string())
        );
    }

    #[test]
    fn v4_mapped_host_rule_is_folded_into_v4() {
        let s = set(&["::ffff:203.0.113.7"]);
        assert!(s.contains(ip("203.0.113.7")));
        assert!(!s.contains(ip("203.0.113.8")));
    }

    #[test]
    fn overlapping_and_adjacent_ranges_merge() {
        let s = set(&["10.0.0.0/9", "10.0.0.0/8", "10.128.0.0/9", "11.0.0.0/8"]);
        assert_eq!(s.v4.len(), 1, "10/8 and 11/8 are adjacent and merge");
        assert!(s.contains(ip("10.0.0.0")));
        assert!(s.contains(ip("11.255.255.255")));
        assert!(!s.contains(ip("12.0.0.0")));
        assert!(!s.contains(ip("9.255.255.255")));
    }

    #[test]
    fn disjoint_ranges_stay_separate() {
        let s = set(&["192.168.0.0/16", "10.0.0.0/8", "172.16.0.0/12"]);
        assert_eq!(s.v4.len(), 3);
        assert!(s.contains(ip("172.31.255.255")));
        assert!(!s.contains(ip("172.32.0.0")));
        assert!(!s.contains(ip("192.167.255.255")));
    }

    #[test]
    fn range_ending_at_address_space_top_does_not_overflow() {
        let s = set(&["255.255.255.0/24", "255.255.0.0/16"]);
        assert!(s.contains(ip("255.255.255.255")));
        assert!(s.contains(ip("255.255.0.0")));
        assert!(!s.contains(ip("255.254.255.255")));
        let s6 = set(&["ffff:ffff:ffff:ffff:ffff:ffff:ffff:ff00/120", "::/0"]);
        assert!(s6.contains(ip("ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff")));
    }

    #[test]
    fn mixed_families_are_independent() {
        let s = set(&["10.0.0.0/8", "2001:db8::/32"]);
        assert!(s.contains(ip("10.0.0.1")));
        assert!(s.contains(ip("2001:db8:1::1")));
        assert!(!s.contains(ip("2001:db9::1")));
        assert!(!s.contains(ip("11.0.0.1")));
    }

    #[test]
    fn host_bits_set_is_rejected_with_a_suggestion() {
        match parse_cidr("10.0.0.5/8") {
            Err(CidrError::HostBitsSet {
                suggested, prefix, ..
            }) => {
                assert_eq!(suggested, "10.0.0.0/8");
                assert_eq!(prefix, 8);
            }
            other => panic!("expected HostBitsSet, got {other:?}"),
        }
        assert!(matches!(
            parse_cidr("2001:db8::1/32"),
            Err(CidrError::HostBitsSet { .. })
        ));
    }

    #[test]
    fn malformed_input_is_rejected() {
        for bad in [
            "",
            "   ",
            "not-an-ip",
            "10.0.0.0/33",
            "2001:db8::/129",
            "10.0.0.0/",
            "10.0.0/8",
            "10.0.0.0/8/8",
            "10.0.0.0 /8",
            "10.0.0.0/-1",
            "fe80::1%eth0",
            "1.2.3.4:80",
            "256.0.0.0/8",
        ] {
            assert!(
                matches!(parse_cidr(bad), Err(CidrError::Invalid { .. })),
                "{bad:?} must be rejected"
            );
        }
    }

    #[test]
    fn overlong_input_is_rejected_without_echoing_all_of_it() {
        let long = "1".repeat(500);
        match parse_cidr(&long) {
            Err(CidrError::Invalid { input, .. }) => assert!(input.len() <= 64),
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn surrounding_whitespace_is_trimmed() {
        assert_eq!(
            parse_cidr("  10.0.0.0/8 ").map(|n| n.to_string()),
            Ok("10.0.0.0/8".to_string())
        );
    }

    #[test]
    fn canonical_form_is_stable() {
        for (input, canonical) in [
            ("203.0.113.7", "203.0.113.7/32"),
            ("2001:DB8::7", "2001:db8::7/128"),
            ("100.64.0.0/10", "100.64.0.0/10"),
        ] {
            assert_eq!(
                parse_cidr(input).map(|n| format!("{}/{}", n.network(), n.prefix())),
                Ok(canonical.to_string())
            );
        }
    }
}
