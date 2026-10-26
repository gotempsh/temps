// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! How visitors arrive at each address of an environment (ADR 064 D9).
//!
//! The IP access policy needs to know whether an environment's visitors come
//! through a tunnel: a tunnel hands the proxy the visitor's internet address,
//! so an allow list of only private ranges would refuse everyone. The ingress
//! plugin knows the answer and implements [`EnvironmentWaysIn`]; the proxy
//! only reads it. When nothing implements it the check is skipped, which is
//! what a binary without the ingress plugin always did.

use std::net::IpAddr;

use async_trait::async_trait;
use thiserror::Error;

/// How visitors reach one address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WayInKind {
    /// Through the named tunnel: the proxy sees the visitor's own address.
    Tunnel { name: String },
    /// Through the operator's load balancer in front of a `behind_proxy`
    /// worker (ADR 058): the proxy sees the visitor's address too.
    BehindProxy { node: String },
    /// Straight to a Temps proxy (this machine's, or a worker's).
    Direct,
}

impl WayInKind {
    /// What visitors come through, when it hands the proxy their internet
    /// address: `tunnel `homelab``, `the load balancer in front of `edge-1``.
    pub fn proxied_through(&self) -> Option<String> {
        match self {
            WayInKind::Tunnel { name } => Some(format!("tunnel `{name}`")),
            WayInKind::BehindProxy { node } => {
                Some(format!("the load balancer in front of `{node}`"))
            }
            WayInKind::Direct => None,
        }
    }
}

/// Why an allow list of only private ranges and a proxied address never
/// combine (ADR 064 D9), in the words both the IP access policy and the
/// tunnel side use. `through` is [`WayInKind::proxied_through`].
pub fn private_allow_list_message(through: &str, cidr: &str, address: &str) -> String {
    format!(
        "Visitors through {through} arrive with their internet address, so `{cidr}` would \
         block everyone on `{address}`. To limit this app to your tailnet, make that address \
         private instead."
    )
}

/// Address blocks nobody on the internet has: RFC 1918, carrier-grade NAT,
/// loopback, link-local (IPv4); unique-local, loopback, link-local (IPv6).
const PRIVATE_V4: &[(u32, u8)] = &[
    (0x0a00_0000, 8),  // 10.0.0.0/8
    (0xac10_0000, 12), // 172.16.0.0/12
    (0xc0a8_0000, 16), // 192.168.0.0/16
    (0x6440_0000, 10), // 100.64.0.0/10
    (0x7f00_0000, 8),  // 127.0.0.0/8
    (0xa9fe_0000, 16), // 169.254.0.0/16
];
const PRIVATE_V6: &[(u128, u8)] = &[
    (0xfc00_u128 << 112, 7),  // fc00::/7
    (0xfe80_u128 << 112, 10), // fe80::/10
    (1, 128),                 // ::1/128
];

/// Whether every address of `cidr` is one nobody on the internet has: the
/// whole range sits inside one of the private blocks above. `None` when it
/// does not parse.
pub fn cidr_is_private(cidr: &str) -> Option<bool> {
    let (address, prefix) = match cidr.trim().split_once('/') {
        Some((address, prefix)) => (address, Some(prefix.parse::<u8>().ok()?)),
        None => (cidr.trim(), None),
    };
    let ip: IpAddr = address.parse().ok()?;
    let ip = match ip {
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) if prefix.is_none_or(|p| p >= 96) => {
                return cidr_is_private(&match prefix {
                    Some(p) => format!("{v4}/{}", p - 96),
                    None => v4.to_string(),
                });
            }
            _ => IpAddr::V6(v6),
        },
        v4 => v4,
    };
    Some(match ip {
        IpAddr::V4(v4) => {
            let prefix = prefix.unwrap_or(32);
            if prefix > 32 {
                return None;
            }
            let net = u32::from(v4);
            PRIVATE_V4.iter().any(|&(block, len)| {
                let mask = if len == 0 { 0 } else { u32::MAX << (32 - len) };
                prefix >= len && net & mask == block
            })
        }
        IpAddr::V6(v6) => {
            let prefix = prefix.unwrap_or(128);
            if prefix > 128 {
                return None;
            }
            let net = u128::from(v6);
            PRIVATE_V6.iter().any(|&(block, len)| {
                let mask = if len == 0 {
                    0
                } else {
                    u128::MAX << (128 - len)
                };
                prefix >= len && net & mask == block
            })
        }
    })
}

/// An IP access allow list that admits only private ranges, and whose it
/// is, for messages that name the rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrivateAllowList {
    /// `the IP access allow list of environment `production``, or of the
    /// project.
    pub rule: String,
    /// Its first range, for the message.
    pub cidr: String,
}

/// Whether an allow list of `cidrs` admits only private ranges: every entry
/// parses and is private. An empty list is not.
pub fn only_private(cidrs: &[String]) -> bool {
    !cidrs.is_empty() && cidrs.iter().all(|c| cidr_is_private(c) == Some(true))
}

/// One address of an environment and how visitors reach it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddressWayIn {
    pub environment_id: i32,
    pub address: String,
    pub way_in: WayInKind,
}

#[derive(Debug, Error)]
#[error("Could not tell how visitors reach project {project_id}: {reason}")]
pub struct WaysInError {
    pub project_id: i32,
    pub reason: String,
}

#[async_trait]
pub trait EnvironmentWaysIn: Send + Sync {
    /// The addresses visitors use for `environment_id`, or for every
    /// environment of the project when it is `None`. Addresses nobody outside
    /// this machine can use (`localhost`, `localho.st`) are left out.
    async fn ways_in(
        &self,
        project_id: i32,
        environment_id: Option<i32>,
    ) -> Result<Vec<AddressWayIn>, WaysInError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_ranges_are_told_from_public_ones() {
        assert_eq!(cidr_is_private("100.64.0.0/10"), Some(true));
        assert_eq!(cidr_is_private("10.0.0.0/8"), Some(true));
        assert_eq!(cidr_is_private("192.168.1.7"), Some(true));
        assert_eq!(cidr_is_private("fd00::/8"), Some(true));
        // Wider than the private block, so it reaches public addresses.
        assert_eq!(cidr_is_private("100.0.0.0/8"), Some(false));
        assert_eq!(cidr_is_private("0.0.0.0/0"), Some(false));
        assert_eq!(cidr_is_private("93.184.215.0/24"), Some(false));
        assert_eq!(cidr_is_private("::/0"), Some(false));
        assert_eq!(cidr_is_private("::ffff:10.0.0.1"), Some(true));
        assert_eq!(cidr_is_private("172.32.0.0/16"), Some(false));
        assert_eq!(cidr_is_private("not a range"), None);
        assert!(only_private(&["100.64.0.0/10".into(), "10.0.0.0/8".into()]));
        assert!(!only_private(&[
            "100.64.0.0/10".into(),
            "93.184.215.0/24".into()
        ]));
        assert!(!only_private(&[]));
    }

    #[test]
    fn the_message_names_the_way_in_the_range_and_the_address() {
        let through = WayInKind::Tunnel {
            name: "homelab".into(),
        }
        .proxied_through()
        .expect("proxied");
        assert_eq!(
            private_allow_list_message(&through, "100.64.0.0/10", "myapp.example.com"),
            "Visitors through tunnel `homelab` arrive with their internet address, so \
             `100.64.0.0/10` would block everyone on `myapp.example.com`. To limit this app to \
             your tailnet, make that address private instead."
        );
        assert_eq!(WayInKind::Direct.proxied_through(), None);
    }
}
