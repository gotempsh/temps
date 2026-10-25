// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! IPv4 ranges a node already uses outside Temps (ADR 056).
//!
//! A worker reports these with registration and in its heartbeat; the control
//! plane steers compute allocations around them and refuses a cluster pool
//! that would collide with them. Only ranges that touch RFC 1918 space are
//! reported, because a compute pool is always RFC 1918: a public route can
//! never collide with it, and leaving those out keeps the report small on a
//! host with a full routing table.

use crate::config::cidrs_overlap;
use ipnet::Ipv4Net;
use serde::{Deserialize, Serialize};

/// Most ranges stored per node. A host with more private networks than this
/// is reported partially (lowest addresses first); the preflight on the node
/// still checks every route, so a missing entry costs a retry, never safety.
pub const MAX_OCCUPIED_RANGES: usize = 256;

/// Longest device or Docker network name kept per range.
pub const MAX_OCCUPIED_NAME_LEN: usize = 128;

/// RFC 1918 blocks. A compute pool must lie inside one of them.
const PRIVATE_BLOCKS: [Ipv4Net; 3] = [
    Ipv4Net::new_assert(std::net::Ipv4Addr::new(10, 0, 0, 0), 8),
    Ipv4Net::new_assert(std::net::Ipv4Addr::new(172, 16, 0, 0), 12),
    Ipv4Net::new_assert(std::net::Ipv4Addr::new(192, 168, 0, 0), 16),
];

/// What owns an occupied range on the node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OccupiedSource {
    /// A unicast host route on a device that is not Temps'.
    Route,
    /// The subnet of a Docker network other than the overlay's.
    Docker,
}

/// One IPv4 range the node already uses.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct OccupiedRange {
    pub cidr: Ipv4Net,
    pub source: OccupiedSource,
    /// Device name for a route, network name for a Docker network.
    pub name: String,
}

impl OccupiedRange {
    pub fn route(cidr: Ipv4Net, device: impl Into<String>) -> Self {
        Self {
            cidr,
            source: OccupiedSource::Route,
            name: device.into(),
        }
    }

    pub fn docker(cidr: Ipv4Net, network: impl Into<String>) -> Self {
        Self {
            cidr,
            source: OccupiedSource::Docker,
            name: network.into(),
        }
    }

    /// Human description for messages: "host route 172.20.0.0/16 on device
    /// 'br-f6051ccc7abf'" or "Docker network 'shop_default' (172.20.0.0/16)".
    pub fn describe(&self) -> String {
        match self.source {
            OccupiedSource::Route => {
                format!("host route {} on device '{}'", self.cidr, self.name)
            }
            OccupiedSource::Docker => {
                format!("Docker network '{}' ({})", self.name, self.cidr)
            }
        }
    }
}

/// Whether a prefix is worth reporting: not a default or host route, and
/// touching RFC 1918 space.
pub fn is_reportable(cidr: &Ipv4Net) -> bool {
    let prefix_len = cidr.prefix_len();
    if prefix_len == 0 || prefix_len >= 32 {
        return false;
    }
    PRIVATE_BLOCKS
        .iter()
        .any(|block| cidrs_overlap(block, cidr))
}

/// Canonical form used on both sides of the wire: reportable ranges only,
/// names sanitized and capped, sorted, one entry per CIDR, at most
/// [`MAX_OCCUPIED_RANGES`]. Applying it twice changes nothing, so the agent
/// can compare successive reports to decide whether anything changed.
pub fn normalize(ranges: impl IntoIterator<Item = OccupiedRange>) -> Vec<OccupiedRange> {
    let mut ranges: Vec<OccupiedRange> = ranges
        .into_iter()
        .filter(|range| is_reportable(&range.cidr))
        .map(|range| OccupiedRange {
            cidr: range.cidr.trunc(),
            source: range.source,
            name: sanitize_name(&range.name),
        })
        .collect();
    ranges.sort();
    ranges.dedup_by(|later, earlier| later.cidr == earlier.cidr);
    ranges.truncate(MAX_OCCUPIED_RANGES);
    ranges
}

fn sanitize_name(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_OCCUPIED_NAME_LEN)
        .collect();
    if cleaned.is_empty() {
        "unknown".to_owned()
    } else {
        cleaned
    }
}

/// The first occupied range overlapping `subnet` (either direction, so a
/// covering route such as `10.0.0.0/8` counts).
pub fn first_overlap<'a>(
    subnet: &Ipv4Net,
    ranges: &'a [OccupiedRange],
) -> Option<&'a OccupiedRange> {
    ranges
        .iter()
        .find(|range| cidrs_overlap(&range.cidr, subnet))
}

/// Collect this host's occupied ranges: unicast routes on devices not in
/// `owned_devices` and every Docker network except `owned_docker_network`.
///
/// With `docker` absent only routes are collected. A Docker call that fails
/// fails the whole collection, so a daemon that is restarting is never
/// reported as having no networks; the caller keeps its previous report.
pub async fn collect(
    docker: Option<&bollard::Docker>,
    owned_devices: &[&str],
    owned_docker_network: &str,
) -> crate::Result<Vec<OccupiedRange>> {
    let mut ranges = host_routes(owned_devices).await?;
    if let Some(docker) = docker {
        ranges.extend(crate::docker::occupied_docker_subnets(docker, owned_docker_network).await?);
    }
    Ok(normalize(ranges))
}

#[cfg(target_os = "linux")]
async fn host_routes(owned_devices: &[&str]) -> crate::Result<Vec<OccupiedRange>> {
    crate::linux::occupied_routes(owned_devices).await
}

#[cfg(not(target_os = "linux"))]
async fn host_routes(_owned_devices: &[&str]) -> crate::Result<Vec<OccupiedRange>> {
    Ok(Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn net(raw: &str) -> Ipv4Net {
        raw.parse().unwrap()
    }

    #[test]
    fn public_default_and_host_routes_are_not_reported() {
        assert!(!is_reportable(&net("0.0.0.0/0")));
        assert!(!is_reportable(&net("203.0.113.0/24")));
        assert!(is_reportable(&net("172.20.0.0/16")));
        assert!(is_reportable(&net("10.0.0.0/8")));
        // A covering public-and-private supernet still touches RFC 1918.
        assert!(is_reportable(&net("0.0.0.0/1")));
    }

    #[test]
    fn normalize_dedupes_sorts_caps_and_sanitizes() {
        let mut input = vec![
            OccupiedRange::route(net("172.20.0.0/16"), "br-a"),
            OccupiedRange::docker(net("172.20.0.0/16"), "shop_default"),
            OccupiedRange::route(net("10.0.0.0/8"), "wg\u{7}corp"),
            OccupiedRange::route(net("8.8.8.0/24"), "eth0"),
        ];
        for index in 0..400u32 {
            let address = std::net::Ipv4Addr::from(0x0a64_0000 + (index << 8));
            input.push(OccupiedRange::docker(
                Ipv4Net::new(address, 24).unwrap(),
                "x".repeat(300),
            ));
        }
        let normalized = normalize(input);
        assert_eq!(normalized.len(), MAX_OCCUPIED_RANGES);
        assert_eq!(
            normalized[0],
            OccupiedRange::route(net("10.0.0.0/8"), "wgcorp")
        );
        assert!(normalized
            .iter()
            .all(|r| r.name.len() <= MAX_OCCUPIED_NAME_LEN));
        assert!(!normalized.iter().any(|r| r.cidr == net("8.8.8.0/24")));
        assert_eq!(normalize(normalized.clone()), normalized);
    }

    #[test]
    fn a_covering_range_overlaps_a_subnet_inside_it() {
        let ranges = [OccupiedRange::route(net("10.0.0.0/8"), "wg-corp")];
        assert!(first_overlap(&net("10.202.3.0/24"), &ranges).is_some());
        assert!(first_overlap(&net("172.20.3.0/24"), &ranges).is_none());
    }

    #[test]
    fn wire_form_is_stable() {
        let range = OccupiedRange::docker(net("172.20.0.0/16"), "shop_default");
        assert_eq!(
            serde_json::to_value(&range).unwrap(),
            serde_json::json!({"cidr": "172.20.0.0/16", "source": "docker", "name": "shop_default"})
        );
    }
}
