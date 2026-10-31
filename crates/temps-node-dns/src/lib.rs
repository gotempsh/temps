// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Per-node DNS resolver for the internal `*.temps.local` zone (ADR-011).
//!
//! Embedded in `temps-node-agent`. Listens only on the bridge gateway IP so every
//! container on the node sees it as its first nameserver without competing
//! with host-local DNS stubs such as `systemd-resolved` on `127.0.0.53`.
//! Operators can query the bridge address explicitly when debugging.
//!
//! The resolver is fully decoupled from the control plane's database. It:
//!
//! 1. Reads zone state from a local in-memory [`ZoneStore`].
//! 2. Persists every applied generation to disk (`zone.json`) so a restart
//!    serves stale-but-correct records before the first sync completes.
//! 3. Long-polls `GET /internal/nodes/{id}/dns/changes?since=N` against the
//!    control plane via [`SyncClient`], applying diffs (or full snapshots)
//!    and ACKing back via `POST /internal/nodes/{id}/dns/ack`.
//! 4. Serves DNS over UDP and TCP via `hickory-server`.
//!
//! Failure model: every failure mode is "keep serving stale". Control plane
//! down = serve last snapshot. Disk write fails = log and keep serving from
//! memory. Bind fails = log and exit (the agent continues without DNS).

pub mod authority;
pub mod config;
pub mod error;
#[cfg(feature = "sync")]
pub mod handle;
pub mod host_dns;
pub mod probe;
pub mod record;
#[cfg(feature = "sync")]
pub mod sync_client;
pub mod upstream;
pub mod zone_store;

pub use authority::{ZoneAuthority, RESOLVER_MARKER_NAME, RESOLVER_MARKER_VALUE};
pub use config::ResolverConfig;
pub use error::ResolverError;
#[cfg(feature = "sync")]
pub use handle::{ResolverHandle, ResolverStatus};
pub use record::{OwnerKind, RecordKind, ZoneRecord};
#[cfg(feature = "sync")]
pub use sync_client::{SyncClient, SyncStatus};
pub use zone_store::ZoneStore;

pub type Result<T> = std::result::Result<T, ResolverError>;

/// Shared slot the deployer reads to write the resolver IP into every
/// container's `/etc/resolv.conf` (`HostConfig.dns`). `None` until the
/// resolver answers its readiness probe ([`probe::probe_resolver`]). Same
/// shape as the agent's `overlay_dns_slot`.
pub type OverlayDnsSlot = std::sync::Arc<std::sync::RwLock<Option<std::net::IpAddr>>>;

/// The network policy's DNS view of one node, shared by the node's enforcer,
/// which swaps in a new snapshot after every pass, and the node's resolver,
/// which answers restricted workloads from it (ADR-055 §9). One per node and
/// process: an enforcer and a resolver given different slots enforce nothing.
pub type DnsPolicySlot = std::sync::Arc<arc_swap::ArcSwap<temps_workload_netpol::DnsPolicy>>;

/// A slot answering every source as unrestricted until the enforcer's first
/// pass stores a snapshot.
pub fn open_dns_policy_slot() -> DnsPolicySlot {
    std::sync::Arc::new(arc_swap::ArcSwap::from_pointee(
        temps_workload_netpol::DnsPolicy::open(),
    ))
}
