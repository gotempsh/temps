// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sha2::{Digest, Sha256};
use std::net::IpAddr;

/// The visitor a challenge and a clearance are bound to (ADR 066, D3):
///
/// ```text
/// SHA-256( ip_prefix ‖ 0x00 ‖ user_agent ‖ 0x00 ‖ ja4_or_source )[0..16]
/// ```
///
/// `ip_prefix` is the /24 of an IPv4 address or the /64 of an IPv6 one, so a
/// visitor moving within one network keeps their clearance. Every input comes
/// from the connection and its headers as the ingress sees them.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct VisitorIdentity(pub(crate) [u8; 16]);

impl VisitorIdentity {
    pub fn compute(client_ip: Option<IpAddr>, user_agent: &str, ja4_or_source: &str) -> Self {
        let mut hasher = Sha256::new();
        match client_ip.map(|ip| ip.to_canonical()) {
            Some(IpAddr::V4(v4)) => {
                hasher.update([4u8]);
                hasher.update(&v4.octets()[..3]);
            }
            Some(IpAddr::V6(v6)) => {
                hasher.update([6u8]);
                hasher.update(&v6.octets()[..8]);
            }
            None => hasher.update([0u8]),
        }
        hasher.update([0u8]);
        hasher.update(user_agent.as_bytes());
        hasher.update([0u8]);
        hasher.update(ja4_or_source.as_bytes());
        let digest = hasher.finalize();
        let mut identity = [0u8; 16];
        identity.copy_from_slice(&digest[..16]);
        Self(identity)
    }
}

impl std::fmt::Debug for VisitorIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "VisitorIdentity(")?;
        for byte in &self.0[..4] {
            write!(f, "{byte:02x}")?;
        }
        write!(f, "…)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const UA: &str = "Mozilla/5.0 (X11; Linux x86_64) Firefox/131.0";

    fn ip(s: &str) -> Option<IpAddr> {
        s.parse().ok()
    }

    #[test]
    fn same_ipv4_slash_24_is_the_same_visitor() {
        assert_eq!(
            VisitorIdentity::compute(ip("203.0.113.7"), UA, "tls:a"),
            VisitorIdentity::compute(ip("203.0.113.200"), UA, "tls:a"),
        );
        assert_ne!(
            VisitorIdentity::compute(ip("203.0.113.7"), UA, "tls:a"),
            VisitorIdentity::compute(ip("203.0.114.7"), UA, "tls:a"),
        );
    }

    #[test]
    fn same_ipv6_slash_64_is_the_same_visitor() {
        assert_eq!(
            VisitorIdentity::compute(ip("2001:db8:1:2::1"), UA, "tls:a"),
            VisitorIdentity::compute(ip("2001:db8:1:2:ffff::9"), UA, "tls:a"),
        );
        assert_ne!(
            VisitorIdentity::compute(ip("2001:db8:1:2::1"), UA, "tls:a"),
            VisitorIdentity::compute(ip("2001:db8:1:3::1"), UA, "tls:a"),
        );
    }

    #[test]
    fn ipv4_mapped_ipv6_is_the_ipv4_address() {
        assert_eq!(
            VisitorIdentity::compute(ip("::ffff:203.0.113.7"), UA, "tls:a"),
            VisitorIdentity::compute(ip("203.0.113.7"), UA, "tls:a"),
        );
    }

    #[test]
    fn user_agent_and_fingerprint_are_bound() {
        let base = VisitorIdentity::compute(ip("203.0.113.7"), UA, "tls:a");
        assert_ne!(
            base,
            VisitorIdentity::compute(ip("203.0.113.7"), "curl/8.5.0", "tls:a")
        );
        assert_ne!(
            base,
            VisitorIdentity::compute(ip("203.0.113.7"), UA, "tls:b")
        );
        assert_ne!(base, VisitorIdentity::compute(None, UA, "tls:a"));
    }

    #[test]
    fn separators_keep_fields_apart() {
        assert_ne!(
            VisitorIdentity::compute(None, "ab", "c"),
            VisitorIdentity::compute(None, "a", "bc"),
        );
    }
}
