//! Configuration types — pure data, no kernel calls.
//!
//! These types are shared between the control plane (which allocates them
//! and ships them over the wire) and the worker-side [`crate::NetworkManager`]
//! (which consumes them to drive the kernel data plane).

use ipnet::Ipv4Net;
use serde::{Deserialize, Serialize};
use std::net::IpAddr;
use uuid::Uuid;

use crate::NetworkError;

/// How traffic is carried between worker nodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Transport {
    /// VXLAN encapsulation over UDP. Universal — works through cloud
    /// firewalls, multi-AZ, and across DCs as long as the chosen UDP port is
    /// reachable. Adds ~50 bytes of overhead per packet, so the bridge MTU
    /// should be the underlay MTU minus 50.
    Vxlan {
        /// VXLAN Network Identifier. All nodes in a Temps cluster must use
        /// the same VNI to be on the same overlay.
        vni: u32,
        /// UDP destination port. The IANA-assigned VXLAN port is 4789.
        port: u16,
    },

    /// No encapsulation — kernel routes compute CIDRs natively over the underlay.
    /// Only works when every node has a route to every other node's compute CIDR
    /// (typically: same L2 segment or a cloud private network where you
    /// disable source/dest checks).
    Native,
}

impl Transport {
    /// Recommended bridge MTU for this transport, given an underlay MTU.
    pub fn bridge_mtu(&self, underlay_mtu: u32) -> u32 {
        match self {
            // VXLAN header (8) + outer UDP (8) + outer IPv4 (20) + outer Ethernet (14) = 50.
            // We don't subtract the inner Ethernet because the bridge sees the inner frame.
            Transport::Vxlan { .. } => underlay_mtu.saturating_sub(50),
            Transport::Native => underlay_mtu,
        }
    }
}

/// Resources allocated to a single node by the control-plane allocator.
///
/// This is the per-node configuration the control plane hands the worker on
/// startup or when the cluster reconfigures.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeAlloc {
    /// Which node this allocation is for. Used for logging/telemetry only —
    /// the kernel layer does not care.
    pub node_id: Uuid,
    /// CIDR this node uses for container IPs. Other nodes route this CIDR
    /// to us via the transport.
    pub compute_cidr: Ipv4Net,
    /// Address the node should assign to its bridge — typically the first
    /// usable host in `compute_cidr`. Provided explicitly rather than derived so
    /// that the allocator stays the single source of truth.
    pub bridge_address: IpAddr,
    /// Address other nodes use to reach this one over the underlay. For
    /// Hetzner private networks this is the private IP; for public-internet
    /// deployments it is the routable public IP.
    pub underlay_address: IpAddr,
}

/// One peer node, as seen from the local node's perspective.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Peer {
    pub node_id: Uuid,
    pub compute_cidr: Ipv4Net,
    pub underlay_address: IpAddr,
}

/// Cluster-wide network configuration. One row in the `network_config`
/// table when this lands behind the control plane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkConfig {
    /// Name of the Linux bridge to create on each node. Defaults to
    /// `br-temps0` to make it obvious in `ip link` output.
    pub bridge_name: String,
    /// Name of the Docker bridge network that owns the bridge. Defaults to
    /// `temps0`.
    pub docker_network_name: String,
    /// Transport mode for cross-node traffic.
    pub transport: Transport,
    /// MTU of the underlay network. Defaults to 1500 (standard Ethernet).
    /// Hetzner Cloud private networks use 1450, so override if needed.
    pub underlay_mtu: u32,
    /// Name of the underlay network device VXLAN should use as its parent
    /// (e.g. `eth0`, `enp1s0`, `bond0`). Ignored for [`Transport::Native`].
    pub underlay_dev: String,
    /// Name of the VXLAN device. Defaults to `vxlan-temps0`.
    pub vxlan_dev_name: String,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            bridge_name: "br-temps0".into(),
            docker_network_name: "temps0".into(),
            transport: Transport::Vxlan {
                vni: 42,
                port: 4789,
            },
            underlay_mtu: 1500,
            underlay_dev: "eth0".into(),
            vxlan_dev_name: "vxlan-temps0".into(),
        }
    }
}

impl NetworkConfig {
    /// Validate the config in isolation (without considering peers).
    ///
    /// Per-peer validation lives in [`Self::validate_with`].
    pub fn validate(&self) -> crate::Result<()> {
        if self.bridge_name.is_empty() {
            return Err(NetworkError::InvalidConfig {
                reason: "bridge_name must not be empty".into(),
            });
        }
        if self.bridge_name.len() > 15 {
            // Linux IFNAMSIZ is 16 bytes including the trailing NUL, so the
            // human-visible cap is 15. We catch this here rather than letting
            // netlink fail with a less obvious error message later.
            return Err(NetworkError::InvalidConfig {
                reason: format!(
                    "bridge_name '{}' exceeds the 15-character interface-name limit",
                    self.bridge_name
                ),
            });
        }
        if self.vxlan_dev_name.len() > 15 {
            return Err(NetworkError::InvalidConfig {
                reason: format!(
                    "vxlan_dev_name '{}' exceeds the 15-character interface-name limit",
                    self.vxlan_dev_name
                ),
            });
        }
        if self.docker_network_name.is_empty() {
            return Err(NetworkError::InvalidConfig {
                reason: "docker_network_name must not be empty".into(),
            });
        }
        if self.underlay_mtu < 1280 {
            return Err(NetworkError::InvalidConfig {
                reason: format!(
                    "underlay_mtu {} is below the IPv6 minimum of 1280",
                    self.underlay_mtu
                ),
            });
        }
        if let Transport::Vxlan { port, .. } = self.transport {
            if port == 0 {
                return Err(NetworkError::InvalidConfig {
                    reason: "vxlan port must not be zero".into(),
                });
            }
            if self.underlay_dev.is_empty() {
                return Err(NetworkError::InvalidConfig {
                    reason: "underlay_dev must be set when transport is vxlan".into(),
                });
            }
        }
        Ok(())
    }

    /// Validate the config in the context of an allocation and peer list.
    pub fn validate_with(&self, alloc: &NodeAlloc, peers: &[Peer]) -> crate::Result<()> {
        self.validate()?;

        // The bridge address must live inside the node's own compute CIDR, otherwise
        // containers won't be able to reach their default gateway.
        let bridge_v4 = match alloc.bridge_address {
            IpAddr::V4(v4) => v4,
            IpAddr::V6(_) => {
                return Err(NetworkError::InvalidConfig {
                    reason: "ipv6 bridge addresses are not yet supported".into(),
                })
            }
        };
        if !alloc.compute_cidr.contains(&bridge_v4) {
            return Err(NetworkError::InvalidConfig {
                reason: format!(
                    "bridge_address {} is not inside compute_cidr {}",
                    alloc.bridge_address, alloc.compute_cidr
                ),
            });
        }

        // No peer may overlap our own CIDR.
        for peer in peers {
            if cidrs_overlap(&peer.compute_cidr, &alloc.compute_cidr) {
                return Err(NetworkError::InvalidPeer {
                    node_id: peer.node_id,
                    reason: format!(
                        "peer compute_cidr {} overlaps local compute_cidr {}",
                        peer.compute_cidr, alloc.compute_cidr
                    ),
                });
            }
            if peer.node_id == alloc.node_id {
                return Err(NetworkError::InvalidPeer {
                    node_id: peer.node_id,
                    reason: "peer list contains the local node — strip self-entries on the control plane".into(),
                });
            }
        }

        // No two peers may share a CIDR.
        for (i, a) in peers.iter().enumerate() {
            for b in &peers[i + 1..] {
                if cidrs_overlap(&a.compute_cidr, &b.compute_cidr) {
                    return Err(NetworkError::InvalidPeer {
                        node_id: b.node_id,
                        reason: format!(
                            "peer compute_cidr {} overlaps another peer's compute_cidr {}",
                            b.compute_cidr, a.compute_cidr
                        ),
                    });
                }
            }
        }

        Ok(())
    }
}

/// Two CIDRs overlap iff one contains the other's network address.
pub(crate) fn cidrs_overlap(a: &Ipv4Net, b: &Ipv4Net) -> bool {
    a.contains(&b.network()) || b.contains(&a.network())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;
    use std::str::FromStr;

    fn alloc(cidr: &str, bridge: &str) -> NodeAlloc {
        NodeAlloc {
            node_id: Uuid::nil(),
            compute_cidr: Ipv4Net::from_str(cidr).unwrap(),
            bridge_address: IpAddr::V4(Ipv4Addr::from_str(bridge).unwrap()),
            underlay_address: IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
        }
    }

    fn peer(node_id: u128, cidr: &str, underlay: &str) -> Peer {
        Peer {
            node_id: Uuid::from_u128(node_id),
            compute_cidr: Ipv4Net::from_str(cidr).unwrap(),
            underlay_address: IpAddr::V4(Ipv4Addr::from_str(underlay).unwrap()),
        }
    }

    #[test]
    fn vxlan_mtu_subtracts_overhead() {
        let t = Transport::Vxlan { vni: 1, port: 4789 };
        assert_eq!(t.bridge_mtu(1500), 1450);
    }

    #[test]
    fn native_mtu_is_underlay_mtu() {
        assert_eq!(Transport::Native.bridge_mtu(1500), 1500);
    }

    #[test]
    fn config_validates_default() {
        NetworkConfig::default().validate().unwrap();
    }

    #[test]
    fn config_rejects_long_iface_name() {
        let c = NetworkConfig {
            bridge_name: "this-is-too-long-for-linux".into(),
            ..NetworkConfig::default()
        };
        let err = c.validate().unwrap_err();
        assert!(matches!(err, NetworkError::InvalidConfig { .. }));
    }

    #[test]
    fn config_rejects_low_mtu() {
        let c = NetworkConfig {
            underlay_mtu: 1000,
            ..NetworkConfig::default()
        };
        assert!(matches!(
            c.validate().unwrap_err(),
            NetworkError::InvalidConfig { .. }
        ));
    }

    #[test]
    fn validate_with_rejects_bridge_outside_cidr() {
        let cfg = NetworkConfig::default();
        let a = alloc("172.20.5.0/24", "172.20.6.1");
        assert!(matches!(
            cfg.validate_with(&a, &[]).unwrap_err(),
            NetworkError::InvalidConfig { .. }
        ));
    }

    #[test]
    fn validate_with_rejects_overlapping_peer_cidr() {
        let cfg = NetworkConfig::default();
        let a = alloc("172.20.5.0/24", "172.20.5.1");
        let p = peer(2, "172.20.5.128/25", "10.0.0.2");
        assert!(matches!(
            cfg.validate_with(&a, &[p]).unwrap_err(),
            NetworkError::InvalidPeer { .. }
        ));
    }

    #[test]
    fn validate_with_rejects_self_peer() {
        let cfg = NetworkConfig::default();
        let a = alloc("172.20.5.0/24", "172.20.5.1");
        let mut p = peer(0, "172.20.6.0/24", "10.0.0.2");
        p.node_id = a.node_id;
        assert!(matches!(
            cfg.validate_with(&a, &[p]).unwrap_err(),
            NetworkError::InvalidPeer { .. }
        ));
    }

    #[test]
    fn validate_with_rejects_duplicate_peer_cidrs() {
        let cfg = NetworkConfig::default();
        let a = alloc("172.20.5.0/24", "172.20.5.1");
        let p1 = peer(2, "172.20.6.0/24", "10.0.0.2");
        let p2 = peer(3, "172.20.6.0/24", "10.0.0.3");
        assert!(matches!(
            cfg.validate_with(&a, &[p1, p2]).unwrap_err(),
            NetworkError::InvalidPeer { .. }
        ));
    }

    #[test]
    fn validate_with_accepts_disjoint_peers() {
        let cfg = NetworkConfig::default();
        let a = alloc("172.20.5.0/24", "172.20.5.1");
        let peers = vec![
            peer(2, "172.20.6.0/24", "10.0.0.2"),
            peer(3, "172.20.7.0/24", "10.0.0.3"),
        ];
        cfg.validate_with(&a, &peers).unwrap();
    }

    #[test]
    fn cidrs_overlap_detects_subset() {
        let a = Ipv4Net::from_str("172.20.0.0/16").unwrap();
        let b = Ipv4Net::from_str("172.20.5.0/24").unwrap();
        assert!(cidrs_overlap(&a, &b));
        assert!(cidrs_overlap(&b, &a));
    }

    #[test]
    fn cidrs_overlap_detects_disjoint() {
        let a = Ipv4Net::from_str("172.20.5.0/24").unwrap();
        let b = Ipv4Net::from_str("172.20.6.0/24").unwrap();
        assert!(!cidrs_overlap(&a, &b));
    }

    #[test]
    fn config_roundtrips_through_json() {
        let c = NetworkConfig::default();
        let s = serde_json::to_string(&c).unwrap();
        let back: NetworkConfig = serde_json::from_str(&s).unwrap();
        assert_eq!(c, back);
    }
}
