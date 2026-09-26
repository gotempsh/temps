// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Multi-host container networking for Temps.
//!
//! This crate gives a Temps worker node the kernel + Docker plumbing it needs
//! so containers on different hosts can reach each other by IP. The high-level
//! flow:
//!
//! 1. Control plane allocates a per-node `compute_cidr` (e.g. `172.20.5.0/24`) from
//!    a cluster-wide pool, plus a list of peer nodes with their own CIDRs and
//!    underlay IPs.
//! 2. [`NetworkManager::bootstrap`] creates a Linux bridge, attaches a transport
//!    (currently VXLAN or native routing), installs forward/masquerade rules,
//!    creates the corresponding Docker bridge network, and adds routes for
//!    every peer's CIDR via the transport device.
//! 3. [`NetworkManager::reconcile_peers`] is called whenever the peer list
//!    changes — it diffs current state against desired and adds/removes FDB
//!    entries and routes idempotently.
//! 4. [`NetworkManager::teardown`] removes everything when a node leaves.
//!
//! All operations are idempotent: calling `bootstrap` twice is a no-op, and
//! `reconcile_peers` is safe to call after a partial failure or restart.
//!
//! ## Platform support
//!
//! Kernel data-plane primitives (bridge, VXLAN, FDB, routes, nftables) are
//! Linux-only. On non-Linux targets, the crate still compiles so that pure
//! logic (config types, peer diff, CIDR allocator) can be unit-tested
//! anywhere, but `NetworkManager::bootstrap` will return
//! [`NetworkError::UnsupportedPlatform`].

pub mod config;
pub mod diff;
pub mod docker;
pub mod error;
pub mod manager;
pub mod overlay_routes;

#[cfg(target_os = "linux")]
pub mod linux;

/// Control-plane CIDR allocator + peer-list helpers. Gated behind the
/// `control_plane` feature so worker-only consumers (the agent) don't pull
/// sea-orm into their build.
#[cfg(feature = "control_plane")]
pub mod allocator;
#[cfg(feature = "control_plane")]
pub mod control_plane;

pub use config::{NetworkConfig, NodeAlloc, Peer, Transport};
pub use diff::{PeerDiff, RouteDiff};
pub use error::NetworkError;
pub use manager::NetworkManager;

/// Convenient `Result` alias for the crate.
pub type Result<T> = std::result::Result<T, NetworkError>;

/// Auto-detect the underlay network device — the interface carrying the
/// host's IPv4 default route — instead of assuming a hardcoded name like
/// `eth0`. Cloud providers with predictable interface naming (Hetzner's
/// `enp6s0`, AWS's `ens5`, etc.) never use `eth0`, so relying on that
/// default fails VXLAN bootstrap on most real deployments.
#[cfg(target_os = "linux")]
pub async fn detect_underlay_device() -> Result<String> {
    linux::detect_underlay_device().await
}

#[cfg(target_os = "linux")]
pub async fn preflight_compute_pool_routes(
    config: &NetworkConfig,
    pool: ipnet::Ipv4Net,
) -> Result<()> {
    linux::preflight_compute_pool_routes(config, pool).await
}

#[cfg(not(target_os = "linux"))]
pub async fn preflight_compute_pool_routes(
    _config: &NetworkConfig,
    _pool: ipnet::Ipv4Net,
) -> Result<()> {
    Ok(())
}

/// Detect the MTU of the selected underlay interface. This must be resolved
/// from the actual link rather than assumed to be 1500: VXLAN adds 50 bytes
/// and Linux will reject an overlay MTU larger than the parent can carry.
#[cfg(target_os = "linux")]
pub async fn detect_underlay_mtu(device: &str) -> Result<u32> {
    linux::detect_underlay_mtu(device).await
}

/// Find the interface that owns an operator-provided private/underlay IP.
/// This is more reliable than the default-route device for VLAN and
/// WireGuard based clusters.
#[cfg(target_os = "linux")]
pub async fn detect_device_for_address(address: std::net::IpAddr) -> Result<String> {
    linux::detect_device_for_address(address).await
}

#[cfg(not(target_os = "linux"))]
pub async fn detect_device_for_address(_address: std::net::IpAddr) -> Result<String> {
    Err(NetworkError::UnsupportedPlatform {
        target: std::env::consts::OS,
    })
}

#[cfg(not(target_os = "linux"))]
pub async fn detect_underlay_mtu(_device: &str) -> Result<u32> {
    Err(NetworkError::UnsupportedPlatform {
        target: std::env::consts::OS,
    })
}

#[cfg(not(target_os = "linux"))]
pub async fn detect_underlay_device() -> Result<String> {
    Err(NetworkError::UnsupportedPlatform {
        target: std::env::consts::OS,
    })
}
