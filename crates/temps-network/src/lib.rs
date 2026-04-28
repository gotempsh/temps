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

pub use config::{NetworkConfig, NodeAlloc, Peer, Transport};
pub use diff::{PeerDiff, RouteDiff};
pub use error::NetworkError;
pub use manager::NetworkManager;

/// Convenient `Result` alias for the crate.
pub type Result<T> = std::result::Result<T, NetworkError>;
