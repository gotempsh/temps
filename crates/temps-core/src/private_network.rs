// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Who counts as inside a private network (ADR-115 D8).
//!
//! A private listener admits the ranges the operator gave, or by default the
//! subnet of the interface it listens on. Tailscale is a preset: its
//! interface carries a `/32`, so it admits Tailscale's ranges instead. A
//! range that overlaps one of the machine's container networks is refused,
//! because any app's container can connect to the host's addresses.

use std::net::IpAddr;

pub use ipnet::IpNet;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::worker_ingress::private_block;

/// What a Tailscale listener admits: its IPv4 and IPv6 ranges.
pub const TAILSCALE_ADMIT: [&str; 2] = ["100.64.0.0/10", "fd7a:115c:a1e0::/48"];

/// Interface name prefixes that belong to containers and VMs, never to a
/// network people reach the machine on.
pub const CONTAINER_INTERFACE_PREFIXES: [&str; 12] = [
    "docker", "br-", "veth", "cni", "podman", "virbr", "flannel", "cali", "vxlan", "bridge",
    "vmnet", "temps",
];

/// Whether `name` is a container or VM bridge interface.
pub fn is_container_interface(name: &str) -> bool {
    CONTAINER_INTERFACE_PREFIXES
        .iter()
        .any(|prefix| name.starts_with(prefix))
}

/// Whether an interface address is Tailscale's: `tailscale0` on Linux, or
/// a `utun` host route in `100.64.0.0/10` or Tailscale's IPv6 range on
/// macOS. Other overlays use `100.64.0.0/10` too (NetBird's `wt0`), so the
/// range alone does not say Tailscale.
pub fn is_tailscale_interface(name: &str, address: IpAddr, prefix: u8) -> bool {
    if name.starts_with("tailscale") {
        return true;
    }
    let host_route = prefix == if address.is_ipv4() { 32 } else { 128 };
    name.starts_with("utun") && host_route && crate::tailnet::is_tailscale_ip(address)
}

/// One address of this machine on a private network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalNetwork {
    pub interface: String,
    pub address: IpAddr,
    /// The interface's prefix length (`24` for `10.8.0.1/24`).
    pub prefix: u8,
    /// Tailscale's interface.
    pub tailscale: bool,
}

impl LocalNetwork {
    /// The interface's subnet (`10.8.0.0/24`).
    pub fn subnet(&self) -> Option<IpNet> {
        IpNet::new(self.address, self.prefix)
            .ok()
            .map(|n| n.trunc())
    }
}

/// A network the console can offer: what serving on it would admit, or why
/// it cannot be used.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PrivateNetworkOption {
    pub interface: String,
    pub address: String,
    pub tailscale: bool,
    /// The ranges it would admit by default.
    pub admit: Vec<String>,
    /// Why it cannot be served on without naming ranges, if it cannot.
    pub problem: Option<String>,
}

/// Parse one admitted range: a CIDR or a bare address, inside a private
/// block, never loopback or a whole family.
pub fn parse_admit(text: &str) -> Result<IpNet, String> {
    let text = text.trim();
    let net = match text.parse::<IpNet>() {
        Ok(net) => net.trunc(),
        Err(_) => match text.parse::<IpAddr>() {
            Ok(ip) => IpNet::from(ip),
            Err(_) => {
                return Err(format!(
                    "{text:?} is not a range: give one like 10.8.0.0/24 or an address"
                ))
            }
        },
    };
    // An IPv4-mapped range (`::ffff:192.168.1.0/120`) is its IPv4 range:
    // peers are compared in their IPv4 form, which an IPv6 range never holds.
    let net = match net {
        IpNet::V6(v6) if v6.prefix_len() >= 96 => match v6.network().to_ipv4_mapped() {
            Some(v4) => ipnet::Ipv4Net::new(v4, v6.prefix_len() - 96)
                .map(IpNet::V4)
                .map_err(|_| format!("{net} is not a range"))?,
            None => net,
        },
        other => other,
    };
    let network = canonical(net.network());
    let Some(block) = private_block(network) else {
        return Err(format!(
            "{net} is not a private range. Admit ranges inside 100.64.0.0/10, 10.0.0.0/8, \
             172.16.0.0/12, 192.168.0.0/16 or fd00::/8 only."
        ));
    };
    let block_net: IpNet = block
        .parse()
        .map_err(|_| format!("{block} is not a range"))?;
    // `10.0.0.0/7` starts in a private block but reaches past it.
    if !block_net.contains(&net.broadcast()) && !block_net.contains(&canonical(net.broadcast())) {
        return Err(format!(
            "{net} reaches past the private range {block}. Admit a range inside it."
        ));
    }
    Ok(net)
}

/// Most ranges one private listener admits. Every private address it serves
/// stores and ships the whole list, so it stays short.
pub const MAX_ADMITS: usize = 16;

/// Parse every admitted range, or the first reason one is refused.
pub fn parse_admits(texts: &[String]) -> Result<Vec<IpNet>, String> {
    if texts.len() > MAX_ADMITS {
        return Err(format!(
            "{} ranges were given; a private listener admits at most {MAX_ADMITS}. Admit a \
             wider private range that covers several of them.",
            texts.len()
        ));
    }
    texts.iter().map(|t| parse_admit(t)).collect()
}

/// What a listener on `network` admits when no ranges were given.
pub fn default_admit(network: &LocalNetwork) -> Result<Vec<IpNet>, String> {
    if network.tailscale {
        return Ok(TAILSCALE_ADMIT
            .iter()
            .filter_map(|r| r.parse().ok())
            .collect());
    }
    let host_prefix = if network.address.is_ipv4() { 32 } else { 128 };
    if network.prefix >= host_prefix {
        return Err(format!(
            "{} ({}) has no subnet, so there is nobody to admit by default. Name who may connect \
             with `--admit`, for example the network's range.",
            network.interface, network.address
        ));
    }
    let subnet = network
        .subnet()
        .ok_or_else(|| format!("{} has no valid subnet", network.interface))?;
    parse_admit(&subnet.to_string())
        .map(|net| vec![net])
        .map_err(|why| format!("{}: {why}", network.interface))
}

/// The first container network an admitted range overlaps, as a refusal.
pub fn container_overlap(admit: &[IpNet], containers: &[(String, IpNet)]) -> Option<String> {
    for range in admit {
        for (name, net) in containers {
            if overlaps(range, net) {
                return Some(format!(
                    "{range} includes the container network {name} ({net}): any app's container \
                     could reach private addresses. Admit a narrower range with `--admit`."
                ));
            }
        }
    }
    None
}

/// Admitted ranges as stored on an address: `10.8.0.0/24,fd00:1::/64`.
pub fn admit_text(admit: &[IpNet]) -> String {
    admit
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

/// Whether two ranges share any address.
pub fn overlaps(a: &IpNet, b: &IpNet) -> bool {
    a.contains(&b.network()) || b.contains(&a.network())
}

/// The networks a console can offer, from this machine's private addresses.
pub fn network_options(
    networks: &[LocalNetwork],
    containers: &[(String, IpNet)],
) -> Vec<PrivateNetworkOption> {
    let mut out: Vec<PrivateNetworkOption> = networks
        .iter()
        .filter(|n| {
            !n.address.is_loopback()
                && private_block(n.address).is_some()
                && !is_container_interface(&n.interface)
        })
        .map(|n| {
            let admit = default_admit(n);
            let problem = match &admit {
                Ok(ranges) => container_overlap(ranges, containers),
                Err(why) => Some(why.clone()),
            };
            PrivateNetworkOption {
                interface: n.interface.clone(),
                address: n.address.to_string(),
                tailscale: n.tailscale,
                admit: admit
                    .map(|r| r.iter().map(ToString::to_string).collect())
                    .unwrap_or_default(),
                problem,
            }
        })
        .collect();
    // IPv4 before IPv6 within an interface, Tailscale first.
    out.sort_by(|a, b| {
        let v6 = |o: &PrivateNetworkOption| o.address.contains(':');
        (!a.tailscale, &a.interface, v6(a), &a.address).cmp(&(
            !b.tailscale,
            &b.interface,
            v6(b),
            &b.address,
        ))
    });
    // `--listen tailscale` is one choice, whatever addresses it has.
    let mut seen_tailscale = false;
    out.retain(|o| !o.tailscale || !std::mem::replace(&mut seen_tailscale, true));
    out
}

fn canonical(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(ip),
        v4 => v4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn net(text: &str) -> IpNet {
        text.parse().unwrap()
    }

    fn local(interface: &str, address: &str, prefix: u8, tailscale: bool) -> LocalNetwork {
        LocalNetwork {
            interface: interface.into(),
            address: address.parse().unwrap(),
            prefix,
            tailscale,
        }
    }

    #[test]
    fn a_wireguard_interface_admits_its_own_subnet_not_the_block() {
        let wg = local("wg0", "10.8.0.1", 24, false);
        assert_eq!(default_admit(&wg).unwrap(), vec![net("10.8.0.0/24")]);
    }

    #[test]
    fn tailscale_admits_its_ranges_although_its_interface_is_a_host_route() {
        let ts = local("tailscale0", "100.101.102.103", 32, true);
        assert_eq!(
            default_admit(&ts).unwrap(),
            vec![net("100.64.0.0/10"), net("fd7a:115c:a1e0::/48")]
        );
    }

    #[test]
    fn a_point_to_point_interface_needs_ranges_named() {
        let ptp = local("wg1", "10.9.0.2", 32, false);
        let why = default_admit(&ptp).unwrap_err();
        assert!(why.contains("wg1 (10.9.0.2) has no subnet"), "{why}");
        assert!(why.contains("--admit"), "{why}");
    }

    #[test]
    fn a_listener_admits_a_bounded_number_of_ranges() {
        let ranges =
            |n: usize| -> Vec<String> { (0..n).map(|i| format!("10.{i}.0.0/16")).collect() };
        assert_eq!(parse_admits(&ranges(MAX_ADMITS)).unwrap().len(), MAX_ADMITS);
        let why = parse_admits(&ranges(MAX_ADMITS + 1)).unwrap_err();
        assert!(why.contains("at most 16"), "{why}");
    }

    #[test]
    fn an_ipv4_mapped_range_is_kept_as_its_ipv4_range() {
        let net = parse_admit("::ffff:192.168.1.0/120").unwrap();
        assert_eq!(net, "192.168.1.0/24".parse::<IpNet>().unwrap());
        assert!(net.contains(&"192.168.1.8".parse::<IpAddr>().unwrap()));
        assert_eq!(
            parse_admit("::ffff:10.0.0.7").unwrap(),
            "10.0.0.7/32".parse::<IpNet>().unwrap()
        );
    }

    #[test]
    fn only_private_ranges_can_be_admitted() {
        assert_eq!(parse_admit("10.8.0.0/24").unwrap(), net("10.8.0.0/24"));
        assert_eq!(parse_admit(" 10.8.0.7/24 ").unwrap(), net("10.8.0.0/24"));
        assert_eq!(parse_admit("192.168.1.30").unwrap(), net("192.168.1.30/32"));
        for refused in [
            "0.0.0.0/0",
            "8.8.8.0/24",
            "127.0.0.0/8",
            "::/0",
            "10.0.0.0/7",
            "x",
        ] {
            assert!(parse_admit(refused).is_err(), "{refused}");
        }
        let why = parse_admit("10.0.0.0/7").unwrap_err();
        assert!(why.contains("reaches past"), "{why}");
    }

    #[test]
    fn a_range_over_a_container_network_is_refused() {
        let containers = vec![("bridge".to_string(), net("172.17.0.0/16"))];
        let why = container_overlap(&[net("172.16.0.0/12")], &containers).unwrap();
        assert!(why.contains("172.16.0.0/12 includes the container network bridge (172.17.0.0/16)"));
        assert_eq!(container_overlap(&[net("10.8.0.0/24")], &containers), None);
        // A narrower admitted range inside a container network overlaps too.
        assert!(container_overlap(&[net("172.17.5.0/24")], &containers).is_some());
    }

    #[test]
    fn the_console_is_offered_this_machines_networks_tailscale_first() {
        let options = network_options(
            &[
                local("eth0", "192.168.1.10", 24, false),
                local("docker0", "172.17.0.1", 16, false),
                local("lo", "127.0.0.1", 8, false),
                local("eth1", "203.0.113.7", 24, false),
                local("tailscale0", "fd7a:115c:a1e0::1", 128, true),
                local("tailscale0", "100.101.102.103", 32, true),
                local("wg0", "172.18.0.1", 24, false),
            ],
            &[("app-net".to_string(), net("172.18.0.0/16"))],
        );
        let names: Vec<&str> = options.iter().map(|o| o.interface.as_str()).collect();
        // Tailscale once, by its IPv4 address.
        assert_eq!(names, ["tailscale0", "eth0", "wg0"]);
        assert_eq!(options[0].address, "100.101.102.103");
        assert_eq!(options[1].admit, ["192.168.1.0/24"]);
        assert_eq!(options[1].problem, None);
        assert!(options[2]
            .problem
            .as_deref()
            .unwrap()
            .contains("container network app-net"));
    }

    #[test]
    fn tailscale_is_told_apart_from_other_overlays_on_the_same_range() {
        let ip = |t: &str| t.parse::<IpAddr>().unwrap();
        assert!(is_tailscale_interface(
            "tailscale0",
            ip("100.101.102.103"),
            32
        ));
        assert!(is_tailscale_interface("utun4", ip("100.101.102.103"), 32));
        // NetBird's interface, on the same range.
        assert!(!is_tailscale_interface("wt0", ip("100.92.10.4"), 16));
        assert!(!is_tailscale_interface("utun100", ip("100.92.10.4"), 16));
        assert!(!is_tailscale_interface("wg0", ip("10.8.0.1"), 24));
    }

    #[test]
    fn admitted_ranges_are_stored_as_one_list() {
        assert_eq!(
            admit_text(&[net("10.8.0.0/24"), net("fd00:1::/64")]),
            "10.8.0.0/24,fd00:1::/64"
        );
    }
}
