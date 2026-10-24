// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Linux kernel data-plane orchestration.
//!
//! This module is only compiled on Linux. It composes the lower-level
//! primitives (bridge, vxlan, route, firewall) into the public lifecycle
//! operations [`bootstrap`], [`reconcile_peers`], and [`teardown`] that
//! [`crate::NetworkManager`] calls into.

use crate::config::{NetworkConfig, NodeAlloc, Peer, Transport};
use crate::diff::{PeerDiff, RouteDiff};
use crate::error::NetworkError;
use std::net::IpAddr;
use tracing::{debug, info};

pub mod bridge;
pub mod firewall;
pub mod route;
pub mod sysctl;
pub mod vxlan;

/// Full bring-up: ip_forward, bridge, transport device, peer FDB, routes,
/// firewall rules. Idempotent.
pub async fn bootstrap(
    config: &NetworkConfig,
    alloc: &NodeAlloc,
    peers: &[Peer],
) -> crate::Result<()> {
    sysctl::enable_ip_forward()?;

    let (handle, _conn) = open_handle().await?;

    bridge::ensure(&handle, &config.bridge_name, alloc, config).await?;

    match config.transport {
        Transport::Vxlan { vni, port } => {
            vxlan::ensure(
                &handle,
                &config.vxlan_dev_name,
                &config.underlay_dev,
                vni,
                port,
                config.transport.bridge_mtu(config.underlay_mtu),
            )
            .await?;
            vxlan::enslave_to_bridge(&handle, &config.vxlan_dev_name, &config.bridge_name).await?;
            // Initial FDB population.
            for peer in peers {
                vxlan::add_fdb(&handle, &config.vxlan_dev_name, peer.underlay_address).await?;
            }
        }
        Transport::Native => {
            // Nothing to do — packets flow over the underlay directly.
        }
    }

    // Routes for each peer's compute CIDR. We point them at the
    // *bridge* interface, not the VXLAN device. The bridge has the
    // L3 address (the gateway IP) so the kernel sources ARP from
    // there; routing via the VXLAN device directly leaves the kernel
    // with no IPv4 address on the chosen egress interface and it
    // falls back to the underlay IP for ARP source — which peer
    // workers then drop because it's in the wrong subnet.
    //
    // Traffic still goes over VXLAN: br-temps0 has vxlan-temps0
    // enslaved to it, so packets that hit the bridge with no local
    // veth match egress through the VXLAN device by L2 forwarding.
    let pref_src_v4 = match alloc.bridge_address {
        std::net::IpAddr::V4(v4) => Some(v4),
        std::net::IpAddr::V6(_) => None,
    };
    for peer in peers {
        match config.transport {
            Transport::Vxlan { .. } => {
                route::add_via_dev(&handle, peer.compute_cidr, &config.bridge_name, pref_src_v4)
                    .await?;
            }
            Transport::Native => {
                route::add_via_gateway(&handle, peer.compute_cidr, peer.underlay_address).await?;
            }
        }
    }

    firewall::install_baseline(config, alloc, peers).await?;

    info!(
        bridge = %config.bridge_name,
        cidr = %alloc.compute_cidr,
        peers = peers.len(),
        "linux network bootstrap complete"
    );
    Ok(())
}

/// Apply peer changes idempotently. Returns true when the kernel state
/// changed.
pub async fn reconcile_peers(
    config: &NetworkConfig,
    alloc: &NodeAlloc,
    current: &[Peer],
    desired: &[Peer],
) -> crate::Result<bool> {
    let peer_diff = PeerDiff::compute(current, desired);
    let route_diff = RouteDiff::compute(current, desired);

    if peer_diff.is_noop() && route_diff.is_noop() {
        // Do not rewrite nftables every five seconds in production. The
        // generation marker lets us cheaply detect a flushed/replaced owned
        // table and atomically restore it only when drift occurred.
        if !firewall::baseline_is_current(config, alloc, desired).await? {
            firewall::install_baseline(config, alloc, desired).await?;
            info!("reconcile_peers repaired firewall drift");
        } else {
            debug!("reconcile_peers: nothing to do");
        }
        return Ok(false);
    }

    let (handle, _conn) = open_handle().await?;

    if let Transport::Vxlan { .. } = config.transport {
        // Remove FDB entries for fully-removed peers.
        for underlay in &peer_diff.fdb_to_remove {
            vxlan::remove_fdb(&handle, &config.vxlan_dev_name, *underlay).await?;
        }
        // Replace = remove old, add new.
        for (have, want) in &peer_diff.to_replace {
            if have.underlay_address != want.underlay_address {
                vxlan::remove_fdb(&handle, &config.vxlan_dev_name, have.underlay_address).await?;
                vxlan::add_fdb(&handle, &config.vxlan_dev_name, want.underlay_address).await?;
            }
        }
        // Add FDB for net-new peers.
        for peer in &peer_diff.to_add {
            vxlan::add_fdb(&handle, &config.vxlan_dev_name, peer.underlay_address).await?;
        }
    }

    // Routes.
    for cidr in &route_diff.to_remove {
        route::remove(&handle, *cidr).await?;
    }
    let pref_src_v4 = match alloc.bridge_address {
        std::net::IpAddr::V4(v4) => Some(v4),
        std::net::IpAddr::V6(_) => None,
    };
    for cidr in &route_diff.to_add {
        match config.transport {
            Transport::Vxlan { .. } => {
                // See bootstrap() for why we route via the bridge,
                // not the VXLAN device directly.
                route::add_via_dev(&handle, *cidr, &config.bridge_name, pref_src_v4).await?;
            }
            Transport::Native => {
                let gateway = desired
                    .iter()
                    .find(|p| p.compute_cidr == *cidr)
                    .map(|p| p.underlay_address)
                    .ok_or(NetworkError::InvalidConfig {
                        reason: format!("route diff added cidr {} with no matching peer", cidr),
                    })?;
                route::add_via_gateway(&handle, *cidr, gateway).await?;
            }
        }
    }

    firewall::install_baseline(config, alloc, desired).await?;

    info!(
        added = peer_diff.to_add.len(),
        removed = peer_diff.fdb_to_remove.len(),
        replaced = peer_diff.to_replace.len(),
        routes_added = route_diff.to_add.len(),
        routes_removed = route_diff.to_remove.len(),
        "reconcile_peers applied"
    );
    Ok(true)
}

/// Tear down everything bootstrap created. Idempotent: each step succeeds
/// silently when the resource is already gone.
pub async fn teardown(config: &NetworkConfig) -> crate::Result<()> {
    let (handle, _conn) = open_handle().await?;

    // Order matters: firewall first (no orphan rules referencing missing
    // chains), then transport device, then bridge, then sysctl is left
    // alone (other software may rely on it).
    firewall::remove_baseline(config).await?;

    if let Transport::Vxlan { .. } = config.transport {
        vxlan::remove(&handle, &config.vxlan_dev_name).await?;
    }
    bridge::remove(&handle, &config.bridge_name).await?;

    info!(bridge = %config.bridge_name, "linux network torn down");
    Ok(())
}

/// Release the parts of an overlay that a renamed overlay on this host needs
/// for itself: the VXLAN device, which holds the cluster's VNI and UDP port,
/// and the peer routes through the old bridge, which would shadow the new
/// bridge's. The bridge and its Docker network stay, with whatever containers
/// are attached; they keep local connectivity until they are redeployed onto
/// the new overlay. Idempotent.
pub async fn release_transport(
    config: &NetworkConfig,
    peer_cidrs: &[ipnet::Ipv4Net],
) -> crate::Result<()> {
    let (handle, _conn) = open_handle().await?;
    for cidr in peer_cidrs {
        route::remove_via_dev(&handle, *cidr, &config.bridge_name).await?;
    }
    if let Transport::Vxlan { .. } = config.transport {
        vxlan::remove(&handle, &config.vxlan_dev_name).await?;
    }
    info!(
        bridge = %config.bridge_name,
        vxlan = %config.vxlan_dev_name,
        routes = peer_cidrs.len(),
        "released the previous overlay's VXLAN device and peer routes"
    );
    Ok(())
}

/// Auto-detect the underlay device from the host's IPv4 default route. See
/// [`route::default_route_device`] for why this replaces a hardcoded
/// `eth0` guess.
pub async fn detect_underlay_device() -> crate::Result<String> {
    let (handle, _conn) = open_handle().await?;
    route::default_route_device(&handle).await
}

/// Read the MTU advertised by the selected underlay link. The agent uses
/// this as the upper bound for its overlay instead of assuming Ethernet's
/// usual 1500-byte MTU; VLANs, WireGuard, and provider private networks often
/// expose a smaller value.
pub async fn detect_underlay_mtu(device: &str) -> crate::Result<u32> {
    let (handle, _conn) = open_handle().await?;
    bridge::link_mtu_by_name(&handle, device)
        .await?
        .ok_or_else(|| NetworkError::UnderlayMtuDetection {
            device: device.to_string(),
            reason: "interface does not exist or did not publish an MTU".to_string(),
        })
}

/// Resolve the Linux interface that owns `address`. The kernel's own address
/// table (`getifaddrs`) is used deliberately: it handles VLAN, bond and
/// WireGuard interfaces uniformly, while default-route discovery selects the
/// public NIC on many hosts. Read in-process, so it needs no `ip` binary and
/// no capability.
pub async fn detect_device_for_address(address: IpAddr) -> crate::Result<String> {
    use nix::sys::socket::SockaddrLike;

    let entries = nix::ifaddrs::getifaddrs().map_err(|error| NetworkError::UnderlayDetection {
        reason: format!("listing interface addresses: {error}"),
    })?;
    for entry in entries {
        let Some(owned) = entry.address.as_ref() else {
            continue;
        };
        let owned: Option<IpAddr> = match owned.family() {
            Some(nix::sys::socket::AddressFamily::Inet) => {
                owned.as_sockaddr_in().map(|a| IpAddr::V4(a.ip()))
            }
            Some(nix::sys::socket::AddressFamily::Inet6) => {
                owned.as_sockaddr_in6().map(|a| IpAddr::V6(a.ip()))
            }
            _ => None,
        };
        if owned == Some(address) {
            return Ok(entry.interface_name);
        }
    }
    Err(NetworkError::UnderlayDetection {
        reason: format!("no interface owns configured address {address}"),
    })
}

/// Reject overlay subnets that would shadow an existing host/VLAN/VPN route.
///
/// `routed` is what this host actually routes on the overlay: its own subnet
/// and every peer's (ADR 056). A route elsewhere in the cluster pool is not a
/// collision; a route over one of these subnets, or one covering it such as
/// `10.0.0.0/8`, is. Routes owned by the configured Temps bridge/VXLAN are
/// accepted so an idempotent restart can reconcile a running overlay.
pub async fn preflight_compute_routes(
    config: &NetworkConfig,
    routed: &[ipnet::Ipv4Net],
) -> crate::Result<()> {
    if routed.is_empty() {
        return Ok(());
    }
    match route_overlapping(routed, &[&config.bridge_name, &config.vxlan_dev_name]).await? {
        Some((subnet, existing_cidr, device)) => Err(NetworkError::HostRouteCollision {
            subnet,
            existing_cidr,
            device,
        }),
        None => Ok(()),
    }
}

/// Reject a WireGuard mesh pool that would shadow an existing host route.
/// The mesh interface's own connected route is accepted, so a restart of an
/// already-running mesh passes.
pub async fn preflight_mesh_routes(pool: ipnet::Ipv4Net) -> crate::Result<()> {
    match route_overlapping(&[pool], &[crate::mesh::MESH_INTERFACE]).await? {
        Some((_, existing_cidr, device)) => Err(NetworkError::MeshRouteCollision {
            pool,
            existing_cidr,
            device,
        }),
        None => Ok(()),
    }
}

/// The first IPv4 route (any table) overlapping one of `targets` that is not
/// on one of `owned_devices`, with the target it overlaps.
///
/// The dump is streamed: each route is judged as it arrives and the walk stops
/// at the first collision, so memory does not grow with the host's route count
/// (a router or VPN concentrator can carry hundreds of thousands).
async fn route_overlapping(
    targets: &[ipnet::Ipv4Net],
    owned_devices: &[&str],
) -> crate::Result<Option<(ipnet::Ipv4Net, ipnet::Ipv4Net, String)>> {
    let mut found = None;
    walk_unicast_routes(
        |existing| {
            targets.iter().find_map(|target| {
                overlapping_prefix(*target, Some(existing.network()), existing.prefix_len())
                    .map(|_| *target)
            })
        },
        |target, existing, device| match unowned_collision(existing, device, owned_devices) {
            Some((existing, device)) => {
                found = Some((target, existing, device));
                false
            }
            None => true,
        },
    )
    .await?;
    Ok(found)
}

/// Every unicast route on a device not in `owned_devices` that could collide
/// with a compute pool (RFC 1918, not default, not /32), for the occupied
/// ranges a node reports (ADR 056). Stops collecting at a bound well above
/// what is stored, so a full routing table cannot grow memory.
pub async fn occupied_routes(
    owned_devices: &[&str],
) -> crate::Result<Vec<crate::occupied::OccupiedRange>> {
    const COLLECT_BOUND: usize = crate::occupied::MAX_OCCUPIED_RANGES * 16;
    let mut ranges = Vec::new();
    walk_unicast_routes(
        |existing| crate::occupied::is_reportable(&existing).then_some(existing),
        |_, existing, device| {
            if let Some((cidr, device)) = unowned_collision(existing, device, owned_devices) {
                ranges.push(crate::occupied::OccupiedRange::route(cidr, device));
            }
            ranges.len() < COLLECT_BOUND
        },
    )
    .await?;
    Ok(ranges)
}

/// Stream every IPv4 unicast route (all tables). `select` judges the prefix
/// first (default and /32 routes never reach it) and returns a tag for the
/// routes worth a device lookup; `visit` receives the tag, the prefix and the
/// device and returns whether to keep walking.
async fn walk_unicast_routes<T>(
    mut select: impl FnMut(ipnet::Ipv4Net) -> Option<T>,
    mut visit: impl FnMut(T, ipnet::Ipv4Net, Option<&str>) -> bool,
) -> crate::Result<()> {
    use futures::TryStreamExt;
    use netlink_packet_route::route::{RouteAddress, RouteAttribute, RouteType};
    use rtnetlink::RouteMessageBuilder;
    use std::net::Ipv4Addr;

    let (handle, _conn) = open_handle().await?;
    let listing_error = |error: rtnetlink::Error| NetworkError::UnderlayDetection {
        reason: format!("listing ipv4 routes: {error}"),
    };

    // Table 0 (unspecified) asks for every table, as `ip route show table all`
    // does; the builder's default is the main table alone.
    let mut routes = handle
        .route()
        .get(RouteMessageBuilder::<Ipv4Addr>::new().table_id(0).build())
        .execute();

    while let Some(message) = routes.try_next().await.map_err(listing_error)? {
        // Only connected/static unicast routes can shadow a subnet. `local`,
        // `broadcast`, `multicast`, `unreachable` and friends are not routes
        // a packet to the overlay would take.
        if message.header.kind != RouteType::Unicast {
            continue;
        }
        let destination = message.attributes.iter().find_map(|attr| match attr {
            RouteAttribute::Destination(RouteAddress::Inet(address)) => Some(*address),
            _ => None,
        });
        let Some(existing) = route_prefix(destination, message.header.destination_prefix_length)
        else {
            continue;
        };
        // Judge the prefix first: almost every route on a busy host cannot
        // matter, and those never cost a device-name lookup.
        let Some(tag) = select(existing) else {
            continue;
        };
        let device = match message.attributes.iter().find_map(|attr| match attr {
            RouteAttribute::Oif(index) => Some(*index),
            _ => None,
        }) {
            Some(index) => bridge::link_name_by_index(&handle, index)
                .await
                .unwrap_or(None),
            None => None,
        };
        if !visit(tag, existing, device.as_deref()) {
            break;
        }
    }

    Ok(())
}

/// A route's destination as a network, or `None` for default and host (/32)
/// routes, which are never reported.
fn route_prefix(destination: Option<std::net::Ipv4Addr>, prefix_len: u8) -> Option<ipnet::Ipv4Net> {
    let destination = destination?;
    if prefix_len == 0 || prefix_len >= 32 {
        return None;
    }
    ipnet::Ipv4Net::new(destination, prefix_len).ok()
}

/// The route's prefix as a network when it overlaps `pool`, else `None`.
///
/// Default routes and host (/32) routes are never reported. That is what the
/// `ip route show` text this replaced did in effect (`default` is not a CIDR,
/// and a /32 is printed as a bare address), and a restart of an existing node
/// must keep passing exactly the checks it passed before.
fn overlapping_prefix(
    pool: ipnet::Ipv4Net,
    destination: Option<std::net::Ipv4Addr>,
    prefix_len: u8,
) -> Option<ipnet::Ipv4Net> {
    let destination = destination?;
    if prefix_len == 0 || prefix_len >= 32 {
        return None;
    }
    let existing = ipnet::Ipv4Net::new(destination, prefix_len).ok()?;
    let overlaps = pool.contains(&existing.network())
        || pool.contains(&existing.broadcast())
        || existing.contains(&pool.network())
        || existing.contains(&pool.broadcast());
    overlaps.then_some(existing)
}

/// An overlapping route is a collision unless it is on a device the caller
/// owns. A route whose device cannot be resolved is still a collision.
fn unowned_collision(
    existing: ipnet::Ipv4Net,
    device: Option<&str>,
    owned_devices: &[&str],
) -> Option<(ipnet::Ipv4Net, String)> {
    let device = device.unwrap_or("unknown");
    if owned_devices.contains(&device) {
        None
    } else {
        Some((existing, device.to_owned()))
    }
}

/// Helper that opens an rtnetlink connection and spawns its background task
/// onto the current tokio runtime, returning a usable handle.
async fn open_handle() -> crate::Result<(rtnetlink::Handle, tokio::task::JoinHandle<()>)> {
    let (conn, handle, _msgs) = rtnetlink::new_connection().map_err(|e| NetworkError::Io {
        op: "rtnetlink::new_connection",
        path: "(socket)".into(),
        reason: e.to_string(),
    })?;
    let task = tokio::spawn(async move {
        conn.await;
    });
    Ok((handle, task))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    type TestRoute = (Option<Ipv4Addr>, u8, Option<String>);

    /// The streaming check applied to a fixed list of routes.
    fn first_collision(
        pool: ipnet::Ipv4Net,
        owned_devices: &[&str],
        routes: &[TestRoute],
    ) -> Option<(ipnet::Ipv4Net, String)> {
        first_routed_collision(&[pool], owned_devices, routes)
            .map(|(_, existing, device)| (existing, device))
    }

    /// The same check against several routed subnets, as the overlay
    /// preflight runs it.
    fn first_routed_collision(
        targets: &[ipnet::Ipv4Net],
        owned_devices: &[&str],
        routes: &[TestRoute],
    ) -> Option<(ipnet::Ipv4Net, ipnet::Ipv4Net, String)> {
        routes.iter().find_map(|(destination, prefix_len, device)| {
            let existing = route_prefix(*destination, *prefix_len)?;
            let target = targets.iter().find_map(|target| {
                overlapping_prefix(*target, Some(existing.network()), existing.prefix_len())
                    .map(|_| *target)
            })?;
            let (existing, device) = unowned_collision(existing, device.as_deref(), owned_devices)?;
            Some((target, existing, device))
        })
    }

    #[test]
    fn a_route_elsewhere_in_the_pool_does_not_collide_with_routed_subnets() {
        // ADR 056: the pool is 10.202.0.0/16, this node routes its own /24
        // and one peer's. A Compose network at 10.202.9.0/24 is irrelevant.
        let routed: [ipnet::Ipv4Net; 2] = [
            "10.202.0.0/24".parse().unwrap(),
            "10.202.1.0/24".parse().unwrap(),
        ];
        let routes = [route("10.202.9.0/24", "br-f6051ccc7abf")];
        assert_eq!(
            first_routed_collision(&routed, &["br-temps0"], &routes),
            None
        );
    }

    #[test]
    fn a_route_over_the_own_or_a_peer_subnet_collides() {
        let routed: [ipnet::Ipv4Net; 2] = [
            "10.202.0.0/24".parse().unwrap(),
            "10.202.1.0/24".parse().unwrap(),
        ];
        let own = [route("10.202.0.0/24", "br-compose")];
        assert_eq!(
            first_routed_collision(&routed, &["br-temps0"], &own),
            Some((routed[0], routed[0], "br-compose".to_owned()))
        );
        let peer = [route("10.202.1.128/25", "tun0")];
        assert_eq!(
            first_routed_collision(&routed, &["br-temps0"], &peer),
            Some((
                routed[1],
                "10.202.1.128/25".parse().unwrap(),
                "tun0".to_owned()
            ))
        );
    }

    #[test]
    fn a_covering_route_collides_with_routed_subnets() {
        let routed: [ipnet::Ipv4Net; 1] = ["10.202.0.0/24".parse().unwrap()];
        let routes = [route("10.0.0.0/8", "wg-corp")];
        assert_eq!(
            first_routed_collision(&routed, &["br-temps0"], &routes),
            Some((
                routed[0],
                "10.0.0.0/8".parse().unwrap(),
                "wg-corp".to_owned()
            ))
        );
    }

    fn route(cidr: &str, device: &str) -> (Option<Ipv4Addr>, u8, Option<String>) {
        let net: ipnet::Ipv4Net = cidr.parse().unwrap();
        (
            Some(net.network()),
            net.prefix_len(),
            Some(device.to_owned()),
        )
    }

    #[test]
    fn a_route_inside_the_pool_is_reported_with_its_device() {
        let pool: ipnet::Ipv4Net = "10.201.0.0/16".parse().unwrap();
        let routes = [
            route("192.168.1.0/24", "eth0"),
            route("10.201.4.0/24", "tun0"),
        ];
        assert_eq!(
            first_collision(pool, &[], &routes),
            Some(("10.201.4.0/24".parse().unwrap(), "tun0".to_owned()))
        );
    }

    #[test]
    fn a_route_that_contains_the_pool_is_reported() {
        let pool: ipnet::Ipv4Net = "10.201.0.0/16".parse().unwrap();
        let routes = [route("10.0.0.0/8", "wg-corp")];
        assert_eq!(
            first_collision(pool, &[], &routes),
            Some(("10.0.0.0/8".parse().unwrap(), "wg-corp".to_owned()))
        );
    }

    #[test]
    fn routes_on_owned_devices_are_accepted_so_a_restart_passes() {
        let pool: ipnet::Ipv4Net = "10.201.0.0/16".parse().unwrap();
        let routes = [route("10.201.0.0/16", "temps-wg0")];
        assert_eq!(first_collision(pool, &["temps-wg0"], &routes), None);
    }

    #[test]
    fn default_and_host_routes_do_not_count() {
        let pool: ipnet::Ipv4Net = "10.201.0.0/16".parse().unwrap();
        let routes = [
            (None, 0, Some("eth0".to_owned())),
            (Some(Ipv4Addr::UNSPECIFIED), 0, Some("eth0".to_owned())),
            (
                Some(Ipv4Addr::new(10, 201, 0, 7)),
                32,
                Some("tun0".to_owned()),
            ),
        ];
        assert_eq!(first_collision(pool, &[], &routes), None);
    }

    #[test]
    fn a_route_without_a_resolvable_device_is_still_a_collision() {
        let pool: ipnet::Ipv4Net = "10.201.0.0/16".parse().unwrap();
        let routes = [(Some(Ipv4Addr::new(10, 201, 0, 0)), 24, None)];
        assert_eq!(
            first_collision(pool, &["temps-wg0"], &routes),
            Some(("10.201.0.0/24".parse().unwrap(), "unknown".to_owned()))
        );
    }

    #[tokio::test]
    async fn loopback_owns_its_address() {
        assert_eq!(
            detect_device_for_address("127.0.0.1".parse().unwrap())
                .await
                .unwrap(),
            "lo"
        );
    }

    #[tokio::test]
    async fn an_unowned_address_is_reported_as_such() {
        let error = detect_device_for_address("203.0.113.77".parse().unwrap())
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("203.0.113.77"),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn listing_routes_needs_no_ip_binary() {
        // A pool no sane host routes: the dump must succeed and find nothing
        // overlapping, which also exercises the rtnetlink path end to end.
        let pool: ipnet::Ipv4Net = "240.77.0.0/24".parse().unwrap();
        assert_eq!(route_overlapping(&[pool], &[]).await.unwrap(), None);
        // The occupied-range walk must also succeed without the ip binary.
        occupied_routes(&[]).await.unwrap();
    }
}
