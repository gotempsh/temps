//! Linux kernel data-plane orchestration.
//!
//! This module is only compiled on Linux. It composes the lower-level
//! primitives (bridge, vxlan, route, firewall) into the public lifecycle
//! operations [`bootstrap`], [`reconcile_peers`], and [`teardown`] that
//! [`crate::NetworkManager`] calls into.

use crate::config::{NetworkConfig, NodeAlloc, Peer, Transport};
use crate::diff::{PeerDiff, RouteDiff};
use crate::error::NetworkError;
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

    firewall::install_baseline(config, alloc).await?;

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
        debug!("reconcile_peers: nothing to do");
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
