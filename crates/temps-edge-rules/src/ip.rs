// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! IP and CIDR parsing for the `ip` field, and a compiled CIDR set with
//! logarithmic lookup (lists of up to 1,000 ranges).

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// Parses an address the way the console does: optional brackets, an IPv6
/// zone suffix is ignored, and IPv4-mapped IPv6 is reported as IPv4 (the
/// proxy normalizes client IPs the same way).
pub fn parse_ip(raw: &str) -> Option<IpAddr> {
    let s = raw.trim();
    let s = s.strip_prefix('[').unwrap_or(s);
    let s = s.strip_suffix(']').unwrap_or(s);
    if s.is_empty() {
        return None;
    }
    if s.contains(':') {
        let without_zone = s.split('%').next().unwrap_or(s);
        let v6: Ipv6Addr = without_zone.parse().ok()?;
        return Some(match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => IpAddr::V6(v6),
        });
    }
    parse_ipv4(s).map(IpAddr::V4)
}

/// Dotted quad with 1–3 digit octets (leading zeros accepted, as in the
/// console).
fn parse_ipv4(s: &str) -> Option<Ipv4Addr> {
    let mut octets = [0u8; 4];
    let mut count = 0;
    for part in s.split('.') {
        if count == 4 || part.is_empty() || part.len() > 3 {
            return None;
        }
        if !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let value: u16 = part.parse().ok()?;
        if value > 255 {
            return None;
        }
        octets[count] = value as u8;
        count += 1;
    }
    (count == 4).then(|| Ipv4Addr::from(octets))
}

/// Normalizes an address for matching: IPv4-mapped IPv6 becomes IPv4.
pub fn canonical_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => IpAddr::V6(v6),
        },
        v4 => v4,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cidr {
    pub addr: IpAddr,
    pub prefix: u8,
}

/// `addr` or `addr/prefix`. A bare address is a single-host range.
pub fn parse_cidr(raw: &str) -> Option<Cidr> {
    let mut parts = raw.trim().split('/');
    let addr = parse_ip(parts.next()?)?;
    let bits: u8 = if addr.is_ipv4() { 32 } else { 128 };
    let prefix = match parts.next() {
        None => bits,
        Some(text) => {
            if text.is_empty() || text.len() > 3 || !text.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            let p: u16 = text.parse().ok()?;
            if p > u16::from(bits) {
                return None;
            }
            p as u8
        }
    };
    if parts.next().is_some() {
        return None;
    }
    Some(Cidr { addr, prefix })
}

fn range_v4(addr: Ipv4Addr, prefix: u8) -> (u32, u32) {
    let value = u32::from(addr);
    let host_bits = 32 - u32::from(prefix);
    let mask = if host_bits == 32 {
        0
    } else {
        u32::MAX << host_bits
    };
    let start = value & mask;
    (start, start | !mask)
}

fn range_v6(addr: Ipv6Addr, prefix: u8) -> (u128, u128) {
    let value = u128::from(addr);
    let host_bits = 128 - u32::from(prefix);
    let mask = if host_bits == 128 {
        0
    } else {
        u128::MAX << host_bits
    };
    let start = value & mask;
    (start, start | !mask)
}

/// Sorted, merged ranges per address family.
#[derive(Debug, Clone, Default)]
pub struct CidrSet {
    v4: Vec<(u32, u32)>,
    v6: Vec<(u128, u128)>,
}

fn merge<T: Ord + Copy + num_like::Succ>(mut ranges: Vec<(T, T)>) -> Vec<(T, T)> {
    ranges.sort_unstable();
    let mut merged: Vec<(T, T)> = Vec::with_capacity(ranges.len());
    for (start, end) in ranges {
        if let Some(last) = merged.last_mut() {
            if start <= last.1.succ_saturating() {
                if end > last.1 {
                    last.1 = end;
                }
                continue;
            }
        }
        merged.push((start, end));
    }
    merged
}

mod num_like {
    pub trait Succ {
        fn succ_saturating(self) -> Self;
    }
    impl Succ for u32 {
        fn succ_saturating(self) -> Self {
            self.saturating_add(1)
        }
    }
    impl Succ for u128 {
        fn succ_saturating(self) -> Self {
            self.saturating_add(1)
        }
    }
}

fn contains<T: Ord + Copy>(ranges: &[(T, T)], value: T) -> bool {
    // First range whose start is greater than `value`; the candidate is the
    // one before it.
    let index = ranges.partition_point(|(start, _)| *start <= value);
    index > 0 && value <= ranges[index - 1].1
}

impl CidrSet {
    pub fn new(cidrs: impl IntoIterator<Item = Cidr>) -> Self {
        let mut v4 = Vec::new();
        let mut v6 = Vec::new();
        for cidr in cidrs {
            match cidr.addr {
                IpAddr::V4(a) => v4.push(range_v4(a, cidr.prefix)),
                IpAddr::V6(a) => v6.push(range_v6(a, cidr.prefix)),
            }
        }
        CidrSet {
            v4: merge(v4),
            v6: merge(v6),
        }
    }

    pub fn contains(&self, ip: IpAddr) -> bool {
        match canonical_ip(ip) {
            IpAddr::V4(a) => contains(&self.v4, u32::from(a)),
            IpAddr::V6(a) => contains(&self.v6, u128::from(a)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        parse_ip(s).unwrap_or_else(|| panic!("{s} should parse"))
    }

    fn set(entries: &[&str]) -> CidrSet {
        CidrSet::new(
            entries
                .iter()
                .map(|e| parse_cidr(e).unwrap_or_else(|| panic!("{e}"))),
        )
    }

    #[test]
    fn parses_v4_v6_and_mapped() {
        assert_eq!(
            ip("203.0.113.7"),
            "203.0.113.7"
                .parse::<IpAddr>()
                .unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED))
        );
        assert!(ip("2001:db8::1").is_ipv6());
        assert_eq!(ip("::ffff:203.0.113.7"), ip("203.0.113.7"));
        assert_eq!(ip("[2001:db8::1]"), ip("2001:db8::1"));
        assert_eq!(ip("fe80::1%eth0"), ip("fe80::1"));
        assert_eq!(ip("010.0.0.1"), ip("10.0.0.1"));
    }

    #[test]
    fn rejects_garbage() {
        for bad in [
            "",
            "1.2.3",
            "1.2.3.4.5",
            "256.1.1.1",
            "1.2.3.x",
            "::g",
            "1:2:3",
            "abcd",
        ] {
            assert!(parse_ip(bad).is_none(), "{bad} should not parse");
        }
        for bad in [
            "1.2.3.4/33",
            "2001:db8::/129",
            "1.2.3.4/",
            "1.2.3.4/a",
            "1.2.3.4/8/9",
        ] {
            assert!(parse_cidr(bad).is_none(), "{bad} should not parse");
        }
    }

    #[test]
    fn matches_ranges_by_family() {
        let s = set(&["203.0.113.0/24", "198.51.100.7", "2001:db8::/32"]);
        assert!(s.contains(ip("203.0.113.255")));
        assert!(!s.contains(ip("203.0.114.0")));
        assert!(s.contains(ip("198.51.100.7")));
        assert!(!s.contains(ip("198.51.100.8")));
        assert!(s.contains(ip("2001:db8:ffff::1")));
        assert!(!s.contains(ip("2001:db9::1")));
        // IPv4-mapped IPv6 matches IPv4 ranges.
        assert!(s.contains(
            "::ffff:203.0.113.9"
                .parse::<IpAddr>()
                .unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED))
        ));
    }

    #[test]
    fn merges_overlapping_and_whole_space_ranges() {
        let s = set(&["10.0.0.0/8", "10.1.0.0/16", "11.0.0.0/8", "0.0.0.0/0"]);
        assert!(s.contains(ip("192.0.2.1")));
        assert!(!s.contains(ip("2001:db8::1")));
        let all6 = set(&["::/0"]);
        assert!(all6.contains(ip("2001:db8::1")));
        assert!(!all6.contains(ip("192.0.2.1")));
    }

    #[test]
    fn many_ranges_stay_correct() {
        let entries: Vec<String> = (0..250).map(|i| format!("10.{i}.0.0/16")).collect();
        let s = CidrSet::new(entries.iter().filter_map(|e| parse_cidr(e)));
        assert!(s.contains(ip("10.249.3.4")));
        assert!(!s.contains(ip("10.250.0.1")));
    }
}
