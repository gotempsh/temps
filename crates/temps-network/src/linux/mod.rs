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

/// Resolve the Linux interface that owns `address`. `ip -o addr` is used
/// deliberately: it handles VLAN, bond and WireGuard interfaces uniformly,
/// while default-route discovery selects the public NIC on many hosts.
pub async fn detect_device_for_address(address: IpAddr) -> crate::Result<String> {
    let output = tokio::process::Command::new("ip")
        .args(["-o", "addr", "show"])
        .output()
        .await
        .map_err(|error| NetworkError::Io {
            op: "ip -o addr show",
            path: "ip".into(),
            reason: error.to_string(),
        })?;
    if !output.status.success() {
        return Err(NetworkError::UnderlayDetection {
            reason: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    let needle = address.to_string();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.len() < 4 {
            continue;
        }
        let owns_address = fields
            .iter()
            .any(|field| field.split('/').next() == Some(needle.as_str()));
        if owns_address {
            return Ok(fields[1]
                .trim_end_matches(':')
                .split('@')
                .next()
                .unwrap_or(fields[1])
                .to_owned());
        }
    }
    Err(NetworkError::UnderlayDetection {
        reason: format!("no interface owns configured address {address}"),
    })
}

/// Reject an overlay pool that would shadow an existing host/VLAN/VPN route.
/// Routes owned by the configured Temps bridge/VXLAN are accepted so an
/// idempotent restart can reconcile an already-running overlay.
pub async fn preflight_compute_pool_routes(
    config: &NetworkConfig,
    pool: ipnet::Ipv4Net,
) -> crate::Result<()> {
    use std::str::FromStr;

    let output = tokio::process::Command::new("ip")
        .args(["-4", "route", "show", "table", "all"])
        .output()
        .await
        .map_err(|error| NetworkError::Io {
            op: "ip -4 route show table all",
            path: "ip".into(),
            reason: error.to_string(),
        })?;
    if !output.status.success() {
        return Err(NetworkError::UnderlayDetection {
            reason: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }

    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        let Some(destination) = fields.first() else {
            continue;
        };
        let Ok(existing) = ipnet::Ipv4Net::from_str(destination) else {
            // `default`, `local`, `broadcast`, and `unreachable` entries do
            // not put a CIDR in the first field and are not connected routes.
            continue;
        };
        let device = fields
            .windows(2)
            .find_map(|pair| (pair[0] == "dev").then_some(pair[1]))
            .unwrap_or("unknown");
        if device == config.bridge_name || device == config.vxlan_dev_name {
            continue;
        }
        if pool.contains(&existing.network())
            || pool.contains(&existing.broadcast())
            || existing.contains(&pool.network())
            || existing.contains(&pool.broadcast())
        {
            return Err(NetworkError::HostRouteCollision {
                pool,
                existing_cidr: existing,
                device: device.to_owned(),
            });
        }
    }
    Ok(())
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
