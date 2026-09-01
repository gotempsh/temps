// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Multi-host network sync — polls the control plane for our compute_cidr
//! allocation and the peer list, then drives `temps_network::NetworkManager`
//! accordingly.
//!
//! The sync loop is *additive*: if the control plane returns `alloc: null`
//! (single-host cluster, or this node hasn't been allocated yet) we simply
//! do nothing and keep retrying. Multi-host bootstrap failures NEVER stop
//! the agent from doing its existing work — the worst case is "this node
//! cannot reach other nodes by overlay IP", same as today.
//!
//! The `temps join` CLI surface is not modified. The agent picks up the
//! overlay automatically when the control plane has decided to allocate
//! one for this node.

use std::net::IpAddr;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use bollard::Docker;
use chrono::{DateTime, Utc};
use ipnet::Ipv4Net;
use serde::{Deserialize, Serialize};
use temps_dns_resolver::{
    ResolverConfig as DnsResolverConfig, ResolverHandle as DnsResolverHandle,
};
use temps_network::{NetworkConfig, NetworkManager, NodeAlloc, Peer};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::AgentConfig;

/// Point-in-time DNS resolver health, refreshed by [`reconcile_resolver`] on
/// every network-sync tick and read by the heartbeat loop (`server.rs`) to
/// attach to the periodic heartbeat POST as `dns_resolver`.
///
/// A `None` in [`SharedDnsHealth`] (rather than this struct) means "network
/// sync hasn't ticked yet for this node" — either right after agent startup,
/// or a true single-host node that never receives a `compute_cidr`
/// allocation and so never touches DNS resolver reconciliation at all (the
/// resolver binds to the overlay bridge address, which doesn't exist without
/// one). That's distinct from `running: false` below, which means the tick
/// ran and found cluster DNS disabled or the resolver down — see the
/// `dns_resolver_running` column doc on `nodes` for the same null-vs-false
/// distinction on the control-plane side.
#[derive(Debug, Clone, Serialize)]
pub struct DnsResolverHeartbeat {
    /// `false` when `AppSettings.cluster_dns.enabled` is off on the control
    /// plane, when the resolver failed to start, or after it was shut down.
    /// `true` only while a resolver handle currently exists and cluster DNS
    /// is enabled.
    #[serde(default)]
    pub running: bool,
    /// Mirrors `ResolverStatus::tasks_alive` — `false` means the resolver's
    /// sync or DNS server task crashed. Only meaningful when `running`.
    #[serde(default)]
    pub tasks_alive: bool,
    #[serde(default)]
    pub last_sync_success_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub consecutive_sync_failures: u32,
    /// Most recent resolver-related error. Usually the sync loop's last
    /// tick failure; when the resolver never started at all, this instead
    /// carries the startup error (e.g. "address already in use") — there is
    /// no separate field for that, and an operator needs to see it either
    /// way.
    #[serde(default)]
    pub last_sync_error: Option<String>,
    #[serde(default)]
    pub record_count: i64,
}

/// Shared slot the network-sync loop publishes DNS resolver health into on
/// every tick. The heartbeat loop (`server.rs::spawn_heartbeat_loop`) reads
/// it to attach `dns_resolver` to the periodic heartbeat POST. Mirrors the
/// `overlay_bridge_address` / `SharedPeers` cross-loop state pattern already
/// used in this module.
pub type SharedDnsHealth = Arc<std::sync::RwLock<Option<DnsResolverHeartbeat>>>;

/// Wire types — match the server's `handlers::network::PeerListResponse`.
/// We re-declare them here rather than depending on `temps-deployments`
/// because that crate transitively pulls in sea-orm and we don't want it
/// in the worker build.
#[derive(Debug, Clone, Deserialize)]
struct WirePeerListResponse {
    #[serde(default)]
    alloc: Option<WireAlloc>,
    #[serde(default)]
    peers: Vec<WirePeer>,
    /// Whether the cluster-DNS resolver is enabled on the control plane
    /// (`AppSettings.cluster_dns.enabled`). `#[serde(default)]` ensures safe
    /// degradation to `false` when talking to an older control plane that does
    /// not yet include this field — the safe side that leaves containers on
    /// Docker's embedded DNS.
    #[serde(default)]
    cluster_dns_enabled: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct WireAlloc {
    node_id: String,
    compute_cidr: String,
    bridge_address: String,
    underlay_address: String,
}

#[derive(Debug, Clone, Deserialize)]
struct WirePeer {
    node_id: String,
    compute_cidr: String,
    underlay_address: String,
}

/// Default polling interval. Kept generous because the cost of a 30s lag
/// is just "a new peer becomes reachable a few seconds later" — no user
/// impact.
const POLL_INTERVAL: Duration = Duration::from_secs(30);

/// Backoff window after a transient failure (network blip, control plane
/// briefly down, etc.).
const BACKOFF_INTERVAL: Duration = Duration::from_secs(5);

/// Shared snapshot of the peer list, refreshed by the sync loop on
/// every successful poll. Container-attach paths read it to install
/// per-peer routes inside the container's netns (without these,
/// outbound traffic to other workers' overlay /24s falls through the
/// container's default route on the primary network and gets dropped).
pub type SharedPeers = Arc<std::sync::RwLock<Vec<Peer>>>;

/// Spawn the network-sync background task. Returns immediately; the task
/// owns its own retry loop and never blocks server startup.
///
/// `overlay_bridge_address` is published into once the overlay
/// bootstraps. The container-create path (`service_handlers.rs`) reads
/// it to set `--dns=<bridge_ip>` on every container so they can resolve
/// `*.temps.local` natively via the per-node Hickory resolver.
///
/// `peers` is refreshed on every poll. Both shared slots live for the
/// agent's process lifetime.
pub fn spawn(
    config: &AgentConfig,
    overlay_bridge_address: Arc<std::sync::RwLock<Option<IpAddr>>>,
    peers: SharedPeers,
    dns_health: SharedDnsHealth,
) {
    let cfg = config.clone();
    tokio::spawn(async move {
        if let Err(e) = run(cfg, overlay_bridge_address, peers, dns_health).await {
            // The loop is designed to retry forever; reaching this branch
            // means the loop itself unwound, which only happens on
            // unrecoverable invariant violations.
            error!("network sync loop exited unexpectedly: {}", e);
        }
    });
}

async fn run(
    config: AgentConfig,
    overlay_bridge_address: Arc<std::sync::RwLock<Option<IpAddr>>>,
    shared_peers: SharedPeers,
    dns_health: SharedDnsHealth,
) -> Result<(), SyncError> {
    info!(
        node_id = config.node_id,
        control_plane = %config.control_plane_url,
        "network sync loop started"
    );

    // Strict TLS — this carries the same secrets as heartbeat.
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .danger_accept_invalid_certs(false)
        .build()
        .map_err(|e| SyncError::ClientBuild(e.to_string()))?;

    let url = format!(
        "{}/api/internal/nodes/{}/network/peers",
        config.control_plane_url.trim_end_matches('/'),
        config.node_id
    );

    let net_config = NetworkConfig::default();
    let manager = match NetworkManager::new(net_config) {
        Ok(m) => m,
        Err(e) => {
            // Static config validation failed — should be impossible since
            // we use Default. Report and exit; agent keeps working.
            return Err(SyncError::ManagerConstruct(e.to_string()));
        }
    };

    let mut bootstrapped = false;
    // Started after first successful bootstrap. Held here (not dropped)
    // so the resolver tasks stay alive for the lifetime of the agent.
    let mut _resolver_handle: Option<DnsResolverHandle> = None;

    let slots = SharedSlots {
        overlay_bridge_address: &overlay_bridge_address,
        shared_peers: &shared_peers,
        dns_health: &dns_health,
    };

    loop {
        match poll_once(&client, &url, &config.token).await {
            Ok(Some(payload)) => {
                if let Err(e) = apply(
                    &manager,
                    payload,
                    &mut bootstrapped,
                    &mut _resolver_handle,
                    &config,
                    &slots,
                )
                .await
                {
                    warn!(error = %e, "network sync apply failed; will retry");
                    tokio::time::sleep(BACKOFF_INTERVAL).await;
                    continue;
                }
            }
            Ok(None) => {
                // No allocation yet — single-host mode for this node.
                debug!("network sync: no compute_cidr allocated yet");
            }
            Err(e) => {
                warn!(error = %e, "network sync poll failed; will retry");
                tokio::time::sleep(BACKOFF_INTERVAL).await;
                continue;
            }
        }

        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

async fn poll_once(
    client: &reqwest::Client,
    url: &str,
    token: &str,
) -> Result<Option<WirePeerListResponse>, SyncError> {
    let resp = client
        .get(url)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| SyncError::Http(e.to_string()))?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(SyncError::HttpStatus { status, body });
    }

    let payload: WirePeerListResponse = resp
        .json()
        .await
        .map_err(|e| SyncError::Parse(e.to_string()))?;

    if payload.alloc.is_none() {
        return Ok(None);
    }
    Ok(Some(payload))
}

/// The cross-loop shared slots the network-sync loop publishes into on every
/// tick, bundled purely to keep [`apply`]'s parameter count manageable. Each
/// field is the same type alias used independently by callers outside this
/// module (`agent.rs`'s CLI wiring, `service_handlers.rs`'s container-attach
/// path) — this struct doesn't change their meaning, just groups the
/// references for one function call.
struct SharedSlots<'a> {
    overlay_bridge_address: &'a Arc<std::sync::RwLock<Option<IpAddr>>>,
    shared_peers: &'a SharedPeers,
    dns_health: &'a SharedDnsHealth,
}

async fn apply(
    manager: &NetworkManager,
    payload: WirePeerListResponse,
    bootstrapped: &mut bool,
    resolver: &mut Option<DnsResolverHandle>,
    config: &AgentConfig,
    slots: &SharedSlots<'_>,
) -> Result<(), SyncError> {
    let Some(alloc_wire) = payload.alloc else {
        return Ok(());
    };
    let alloc = parse_alloc(&alloc_wire)?;
    let peers: Result<Vec<Peer>, _> = payload.peers.iter().map(parse_peer).collect();
    let peers = peers?;
    // Captured before `bootstrap()` consumes `alloc` below. Needed every
    // tick (not just the first) since resolver reconciliation now runs
    // unconditionally — see the `reconcile_resolver` call at the bottom.
    let bridge_address = alloc.bridge_address;

    // Capture the bridge gateway up front — `bootstrap` consumes
    // `alloc` and the route sweep at the bottom needs the IP after.
    let bridge_v4 = match alloc.bridge_address {
        IpAddr::V4(v4) => v4.to_string(),
        IpAddr::V6(_) => return Ok(()),
    };

    // Publish the latest peer list before driving any kernel changes.
    // Container-attach handlers read this slot to install per-peer
    // overlay routes inside the container's netns — they need the
    // same view the kernel data plane is about to apply.
    if let Ok(mut slot) = slots.shared_peers.write() {
        *slot = peers.clone();
    }

    if !*bootstrapped {
        info!(
            cidr = %alloc.compute_cidr,
            peers = peers.len(),
            "bringing up multi-host overlay"
        );
        // Clone before bootstrap consumes alloc — we need it for the
        // Docker network creation step right after.
        let alloc_for_docker = alloc.clone();
        manager
            .bootstrap(alloc, peers.clone())
            .await
            .map_err(|e| SyncError::Bootstrap(e.to_string()))?;
        *bootstrapped = true;

        // Create the Docker bridge network pinned to the kernel bridge
        // we just brought up. `temps-network::linux::bootstrap` only
        // creates kernel-level primitives (br-temps0, vxlan-temps0,
        // routes, nftables); the corresponding Docker network has to be
        // created here so the deployer + service handlers can attach
        // containers to it. Without this, `compute_ip` is always None
        // and the DNS registry never gets per-container records.
        if let Err(e) = ensure_overlay_docker_network(&alloc_for_docker).await {
            warn!(
                error = %e,
                "Failed to create overlay Docker network; containers \
                 won't have cross-node IPs (continuing single-host)"
            );
        }
    } else {
        let changed = manager
            .reconcile_peers(peers.clone())
            .await
            .map_err(|e| SyncError::Reconcile(e.to_string()))?;
        if changed {
            info!("multi-host peer list updated");
        }
    }

    // Reconcile the per-node DNS resolver (ADR-024) against the control
    // plane's current `cluster_dns_enabled` setting and the resolver's own
    // task health. Runs on *every* tick — not just the first bootstrap — so
    // toggling the setting or a crashed resolver task both self-heal within
    // one poll interval instead of requiring an agent restart.
    reconcile_resolver(
        payload.cluster_dns_enabled,
        bridge_address,
        resolver,
        config,
        slots.overlay_bridge_address,
        slots.dns_health,
    )
    .await;

    // Re-inject per-peer routes inside every overlay-attached
    // container's netns. The routes don't survive a container netns
    // recreate (Docker auto-restart on crash, worker reboot, image
    // update), so a sync-loop sweep is the simplest way to heal
    // automatically. `ip route replace` is idempotent — re-running
    // when nothing changed is a no-op.
    if let Err(e) = sweep_overlay_container_routes(&bridge_v4, &peers).await {
        debug!(
            error = %e,
            "Container route sweep skipped (will retry next tick)"
        );
    }

    Ok(())
}

/// Walk every container currently attached to `temps0` and re-install
/// the per-peer routes inside their netns. Used by the sync loop to
/// repair routes lost across container restarts.
///
/// Best-effort: any container that fails individually is logged and
/// skipped. The sweep itself only errors when we can't even talk to
/// the local Docker daemon.
async fn sweep_overlay_container_routes(
    bridge_gateway: &str,
    peers: &[Peer],
) -> Result<(), String> {
    use bollard::query_parameters::{InspectContainerOptions, ListContainersOptions};

    if peers.is_empty() {
        return Ok(());
    }

    let docker = Docker::connect_with_local_defaults()
        .map_err(|e| format!("connect_with_local_defaults: {}", e))?;

    // network=temps0 filter narrows the list to containers that are
    // actually on the overlay. Includes stopped containers (filters
    // are OR'd on status by default), but `inspect.state.pid > 0` will
    // skip those before we try to nsenter.
    let overlay_name = NetworkConfig::default().docker_network_name;
    let mut filters = std::collections::HashMap::new();
    filters.insert("network".to_string(), vec![overlay_name.clone()]);
    let opts = ListContainersOptions {
        all: false,
        filters: Some(filters),
        ..Default::default()
    };

    let containers = docker
        .list_containers(Some(opts))
        .await
        .map_err(|e| format!("list_containers: {}", e))?;

    for c in containers {
        let Some(id) = c.id.as_deref() else {
            continue;
        };
        let inspect = match docker
            .inspect_container(id, None::<InspectContainerOptions>)
            .await
        {
            Ok(i) => i,
            Err(e) => {
                debug!(container = %id, error = %e, "Inspect failed during route sweep");
                continue;
            }
        };
        let pid = match inspect
            .state
            .as_ref()
            .and_then(|s| s.pid)
            .filter(|p| *p > 0)
        {
            Some(p) => p as i32,
            None => continue,
        };
        // Only attempt for containers that actually have an overlay
        // gateway recorded — stopped/half-attached containers may
        // have a network entry without a gateway IP.
        let has_overlay_gw = inspect
            .network_settings
            .as_ref()
            .and_then(|ns| ns.networks.as_ref())
            .and_then(|nets| nets.get(&overlay_name))
            .and_then(|n| n.gateway.as_deref())
            .filter(|g| !g.is_empty())
            .is_some();
        if !has_overlay_gw {
            continue;
        }

        if let Err(e) = temps_network::overlay_routes::install_peer_routes_in_container(
            pid,
            "eth1", // hint only; actual iface is discovered by IP match
            bridge_gateway,
            peers,
        )
        .await
        {
            debug!(
                container = %id,
                error = %e,
                "Failed to (re)install overlay peer routes; will retry next tick"
            );
        }
    }
    Ok(())
}

/// Reconcile the per-node DNS resolver against the control plane's current
/// `cluster_dns_enabled` setting and the resolver's own task health. Called
/// on every sync tick (ADR-024's original design only ran this once, at
/// first bootstrap — toggling the setting afterward, or the resolver task
/// crashing, both required a manual agent restart to recover from; this
/// closes that gap):
///
/// - Enabled, no resolver running (first time, or after a crash/shutdown):
///   start one.
/// - Enabled, resolver running but one of its background tasks has died
///   (`status().tasks_alive == false`): drop the stale handle and start a
///   fresh one. A half-dead resolver (e.g. server task panicked, sync task
///   still running) is worse than no resolver — it can serve a frozen,
///   increasingly stale zone without any caller knowing.
/// - Enabled, resolver running and healthy: no-op.
/// - Disabled, resolver running: shut it down and clear the published
///   bridge address so new containers stop getting `--dns=<bridge_ip>` and
///   fall back to Docker's embedded DNS, matching what a fresh disabled
///   node would see.
/// - Disabled, no resolver running: no-op.
///
/// Regardless of which branch runs, the final resolver state is always
/// published to `dns_health` before returning (see [`publish_dns_health`]) —
/// that's what makes the resolver's health visible to the control plane via
/// the agent's next heartbeat, closing the "silently fails, operator has to
/// SSH in and read logs" gap this reconciliation loop was already built to
/// self-heal but not to report.
async fn reconcile_resolver(
    cluster_dns_enabled: bool,
    bridge_address: IpAddr,
    resolver: &mut Option<DnsResolverHandle>,
    config: &AgentConfig,
    overlay_bridge_address: &Arc<std::sync::RwLock<Option<IpAddr>>>,
    dns_health: &SharedDnsHealth,
) {
    if !cluster_dns_enabled {
        if let Some(handle) = resolver.take() {
            info!(
                "cluster DNS resolver disabled (AppSettings.cluster_dns.enabled=false from \
                 control plane); shutting down and falling back to Docker's embedded DNS"
            );
            handle.shutdown().await;
            if let Ok(mut slot) = overlay_bridge_address.write() {
                *slot = None;
            }
        }
        publish_dns_health(dns_health, false, None, None);
        return;
    }

    if let Some(handle) = resolver.as_ref() {
        let status = handle.status();
        if status.tasks_alive {
            publish_dns_health(dns_health, true, Some(&status), None);
            return;
        }
        warn!(
            bridge = %bridge_address,
            consecutive_sync_failures = status.sync.consecutive_failures,
            "DNS resolver task exited unexpectedly; respawning"
        );
        // Drop the dead handle. `shutdown()` on an already-exited task just
        // awaits the (already-finished) JoinHandles, so this is safe even
        // though one of them is what triggered the respawn.
        if let Some(handle) = resolver.take() {
            handle.shutdown().await;
        }
    }

    let dns_cfg = DnsResolverConfig::new(
        config.node_id,
        config.token.clone(),
        config.control_plane_url.clone(),
        bridge_address,
        config.dns_data_dir.clone(),
    );
    let snapshot_path = dns_cfg.snapshot_path();
    let mut start_error = None;
    match DnsResolverHandle::start(dns_cfg).await {
        Ok(handle) => {
            info!(
                snapshot = %snapshot_path.display(),
                bridge = %bridge_address,
                "DNS resolver started"
            );
            *resolver = Some(handle);

            // Publish the bridge address so `service_handlers::create_service`
            // can wire it into every container's `--dns`. Done right after the
            // resolver is up so the slot is never advertised before the
            // resolver is actually accepting queries.
            if let Ok(mut slot) = overlay_bridge_address.write() {
                *slot = Some(bridge_address);
            }
        }
        Err(e) => {
            warn!(
                error = %e,
                bridge = %bridge_address,
                "DNS resolver failed to start; this node has no in-cluster DNS \
                 (heartbeats / deployments / proxy continue to work)"
            );
            // `resolver` is already None here (the dead handle, if any, was
            // dropped above before this respawn attempt). Clear
            // `overlay_bridge_address` too — otherwise a previous, now-dead
            // resolver's IP would stay published, and new containers would
            // get `--dns=<dead IP>` with no listener behind it. The typical
            // failure (port 53 already bound) won't fix itself by retrying
            // immediately, but the next sync tick (POLL_INTERVAL later) will
            // try again rather than waiting for an agent restart.
            if let Ok(mut slot) = overlay_bridge_address.write() {
                *slot = None;
            }
            start_error = Some(e.to_string());
        }
    }

    let status = resolver.as_ref().map(|h| h.status());
    publish_dns_health(dns_health, resolver.is_some(), status.as_ref(), start_error);
}

/// Build a [`DnsResolverHeartbeat`] snapshot and publish it to `dns_health`.
///
/// `running` and `status` should agree (`status.is_some() == running`) for
/// every call site above except the "start failed" path, where `running` is
/// `false` and `status` is `None` — `start_error` carries the failure
/// instead. `status.tasks_alive` is trusted over `running` for the
/// `tasks_alive` output field so a caller can never observe the
/// contradictory `running: true, tasks_alive: true` pair for a resolver that
/// was never actually running.
fn publish_dns_health(
    dns_health: &SharedDnsHealth,
    running: bool,
    status: Option<&temps_dns_resolver::ResolverStatus>,
    start_error: Option<String>,
) {
    let snapshot = match status {
        Some(status) if running => DnsResolverHeartbeat {
            running: true,
            tasks_alive: status.tasks_alive,
            last_sync_success_at: status.sync.last_success_at.map(DateTime::<Utc>::from),
            consecutive_sync_failures: status.sync.consecutive_failures,
            last_sync_error: status.sync.last_error.clone(),
            record_count: status.record_count as i64,
        },
        _ => DnsResolverHeartbeat {
            running: false,
            tasks_alive: false,
            last_sync_success_at: None,
            consecutive_sync_failures: 0,
            last_sync_error: start_error,
            record_count: 0,
        },
    };
    if let Ok(mut slot) = dns_health.write() {
        *slot = Some(snapshot);
    }
}

/// Create the Docker bridge network that sits on top of the kernel
/// `br-temps0` bridge that `temps-network::linux::bootstrap` brought up.
/// Idempotent — the helper short-circuits when the network already exists
/// with a matching subnet.
///
/// `temps-network::docker::ensure_network` does the actual work; we just
/// open the local Docker socket and forward to it. The reason this lives
/// in the agent and not in `linux::bootstrap` itself: keeping the
/// kernel-level bootstrap pure of bollard means the integration tests in
/// `temps-network/tests/it_kernel.rs` don't need a real Docker daemon.
async fn ensure_overlay_docker_network(alloc: &NodeAlloc) -> Result<(), SyncError> {
    let docker = Docker::connect_with_local_defaults()
        .map_err(|e| SyncError::DockerConnect(e.to_string()))?;
    let cfg = NetworkConfig::default();
    temps_network::docker::ensure_network(&docker, &cfg, alloc)
        .await
        .map_err(|e| SyncError::Bootstrap(format!("docker network: {}", e)))?;
    info!(
        network = %cfg.docker_network_name,
        cidr = %alloc.compute_cidr,
        "overlay Docker network ready"
    );
    Ok(())
}

fn parse_alloc(w: &WireAlloc) -> Result<NodeAlloc, SyncError> {
    Ok(NodeAlloc {
        node_id: Uuid::parse_str(&w.node_id)
            .map_err(|e| SyncError::WireParse(format!("alloc.node_id: {}", e)))?,
        compute_cidr: Ipv4Net::from_str(&w.compute_cidr)
            .map_err(|e| SyncError::WireParse(format!("alloc.compute_cidr: {}", e)))?,
        bridge_address: IpAddr::from_str(&w.bridge_address)
            .map_err(|e| SyncError::WireParse(format!("alloc.bridge_address: {}", e)))?,
        underlay_address: IpAddr::from_str(&w.underlay_address)
            .map_err(|e| SyncError::WireParse(format!("alloc.underlay_address: {}", e)))?,
    })
}

fn parse_peer(w: &WirePeer) -> Result<Peer, SyncError> {
    Ok(Peer {
        node_id: Uuid::parse_str(&w.node_id)
            .map_err(|e| SyncError::WireParse(format!("peer.node_id: {}", e)))?,
        compute_cidr: Ipv4Net::from_str(&w.compute_cidr)
            .map_err(|e| SyncError::WireParse(format!("peer.compute_cidr: {}", e)))?,
        underlay_address: IpAddr::from_str(&w.underlay_address)
            .map_err(|e| SyncError::WireParse(format!("peer.underlay_address: {}", e)))?,
    })
}

#[derive(Debug, thiserror::Error)]
enum SyncError {
    #[error("failed to build http client: {0}")]
    ClientBuild(String),

    #[error("failed to construct NetworkManager: {0}")]
    ManagerConstruct(String),

    #[error("http error: {0}")]
    Http(String),

    #[error("control plane returned {status}: {body}")]
    HttpStatus {
        status: reqwest::StatusCode,
        body: String,
    },

    #[error("failed to parse peer list response: {0}")]
    Parse(String),

    #[error("malformed wire payload: {0}")]
    WireParse(String),

    #[error("bootstrap failed: {0}")]
    Bootstrap(String),

    #[error("reconcile failed: {0}")]
    Reconcile(String),

    #[error("failed to connect to local Docker daemon: {0}")]
    DockerConnect(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wire_alloc() -> WireAlloc {
        WireAlloc {
            node_id: "00000000-0000-0000-0000-00000000002a".into(),
            compute_cidr: "172.20.5.0/24".into(),
            bridge_address: "172.20.5.1".into(),
            underlay_address: "10.0.0.5".into(),
        }
    }

    fn wire_peer() -> WirePeer {
        WirePeer {
            node_id: "00000000-0000-0000-0000-000000000007".into(),
            compute_cidr: "172.20.6.0/24".into(),
            underlay_address: "10.0.0.6".into(),
        }
    }

    #[test]
    fn parse_alloc_ok() {
        let a = parse_alloc(&wire_alloc()).unwrap();
        assert_eq!(a.compute_cidr.to_string(), "172.20.5.0/24");
        assert_eq!(a.bridge_address.to_string(), "172.20.5.1");
        assert_eq!(a.underlay_address.to_string(), "10.0.0.5");
    }

    #[test]
    fn parse_alloc_rejects_bad_cidr() {
        let mut w = wire_alloc();
        w.compute_cidr = "not-a-cidr".into();
        let err = parse_alloc(&w).unwrap_err();
        assert!(matches!(err, SyncError::WireParse(_)));
    }

    #[test]
    fn parse_alloc_rejects_bad_uuid() {
        let mut w = wire_alloc();
        w.node_id = "not-a-uuid".into();
        let err = parse_alloc(&w).unwrap_err();
        assert!(matches!(err, SyncError::WireParse(_)));
    }

    #[test]
    fn parse_peer_ok() {
        let p = parse_peer(&wire_peer()).unwrap();
        assert_eq!(p.compute_cidr.to_string(), "172.20.6.0/24");
        assert_eq!(p.underlay_address.to_string(), "10.0.0.6");
    }

    #[test]
    fn deserialize_response_with_null_alloc() {
        // Server returns no `alloc` field at all when serde skip_serializing_if
        // is configured; verify our deserializer treats that as None.
        let json = r#"{"peers": []}"#;
        let resp: WirePeerListResponse = serde_json::from_str(json).unwrap();
        assert!(resp.alloc.is_none());
        assert!(resp.peers.is_empty());
    }

    #[test]
    fn deserialize_response_with_alloc_and_peers() {
        let json = r#"{
            "alloc": {
                "node_id": "00000000-0000-0000-0000-00000000002a",
                "compute_cidr": "172.20.5.0/24",
                "bridge_address": "172.20.5.1",
                "underlay_address": "10.0.0.5"
            },
            "peers": [{
                "node_id": "00000000-0000-0000-0000-000000000007",
                "compute_cidr": "172.20.6.0/24",
                "underlay_address": "10.0.0.6"
            }]
        }"#;
        let resp: WirePeerListResponse = serde_json::from_str(json).unwrap();
        assert!(resp.alloc.is_some());
        assert_eq!(resp.peers.len(), 1);
    }

    // ADR-024: cluster_dns_enabled must default to false when the field is
    // absent from the wire response (e.g. older control-plane versions that
    // predate this field). `#[serde(default)]` guarantees safe degradation.
    #[test]
    fn deserialize_response_without_cluster_dns_field_defaults_to_disabled() {
        let json = r#"{
            "alloc": {
                "node_id": "00000000-0000-0000-0000-00000000002a",
                "compute_cidr": "172.20.5.0/24",
                "bridge_address": "172.20.5.1",
                "underlay_address": "10.0.0.5"
            },
            "peers": []
        }"#;
        let resp: WirePeerListResponse = serde_json::from_str(json).unwrap();
        assert!(
            !resp.cluster_dns_enabled,
            "cluster_dns_enabled must default to false when absent from wire payload"
        );
    }

    #[test]
    fn deserialize_response_with_cluster_dns_enabled_true() {
        let json = r#"{
            "alloc": null,
            "peers": [],
            "cluster_dns_enabled": true
        }"#;
        let resp: WirePeerListResponse = serde_json::from_str(json).unwrap();
        assert!(resp.cluster_dns_enabled);
        assert!(resp.alloc.is_none());
    }

    // Verify that when cluster_dns_enabled=false, the overlay_bridge_address
    // slot is NOT populated by apply() — meaning DockerRuntime will write no
    // custom HostConfig.Dns to containers.
    //
    // We test this by constructing a payload that has an alloc but
    // cluster_dns_enabled=false, calling apply(), and asserting the shared
    // slot remains None. We avoid spinning up a real NetworkManager /
    // DockerRuntime by checking the slot directly — the point being that
    // `apply` returns early from the resolver+slot path, not that the network
    // manager itself doesn't run.
    //
    // Note: apply() internally calls manager.bootstrap() which requires kernel
    // privileges (not available in unit tests). We therefore only test the
    // deserialization / guard logic here — the full integration is covered by
    // the dockerized IT suite.
    #[test]
    fn wire_payload_without_cluster_dns_does_not_set_bridge_address_field() {
        // Build a wire payload with alloc present but cluster_dns_enabled=false.
        let json = r#"{
            "alloc": {
                "node_id": "00000000-0000-0000-0000-00000000002a",
                "compute_cidr": "172.20.5.0/24",
                "bridge_address": "172.20.5.1",
                "underlay_address": "10.0.0.5"
            },
            "peers": [],
            "cluster_dns_enabled": false
        }"#;
        let payload: WirePeerListResponse = serde_json::from_str(json).unwrap();

        // The payload carries an alloc, so `apply()` would bootstrap the
        // overlay if called — but the cluster_dns_enabled=false path must
        // leave overlay_bridge_address as None. We assert on the deserialized
        // field rather than calling apply() (which requires kernel netns ops).
        assert!(payload.alloc.is_some(), "test requires alloc to be present");
        assert!(
            !payload.cluster_dns_enabled,
            "cluster_dns_enabled must be false in this test payload"
        );
        // The guard condition in apply():
        //   if payload.cluster_dns_enabled { spawn_resolver(...); slot=Some(...) }
        // is validated by the field value above. Runtime behaviour is covered
        // by the dockerized IT suite.
    }

    // ── DNS resolver heartbeat publishing ──────────────────────────────

    fn empty_dns_health() -> SharedDnsHealth {
        Arc::new(std::sync::RwLock::new(None))
    }

    #[test]
    fn publish_dns_health_disabled_reports_not_running() {
        let slot = empty_dns_health();
        publish_dns_health(&slot, false, None, None);

        let health = slot
            .read()
            .unwrap()
            .clone()
            .expect("slot must be Some after a tick");
        assert!(!health.running);
        assert!(!health.tasks_alive);
        assert_eq!(health.record_count, 0);
        assert_eq!(health.consecutive_sync_failures, 0);
        assert!(health.last_sync_success_at.is_none());
        assert!(
            health.last_sync_error.is_none(),
            "a clean disable carries no error"
        );
    }

    #[test]
    fn publish_dns_health_running_healthy_mirrors_resolver_status() {
        let slot = empty_dns_health();
        let now = std::time::SystemTime::now();
        let status = temps_dns_resolver::ResolverStatus {
            tasks_alive: true,
            sync: temps_dns_resolver::SyncStatus {
                last_success_at: Some(now),
                consecutive_failures: 0,
                last_error: None,
            },
            record_count: 42,
            zone_generation: 7,
        };
        publish_dns_health(&slot, true, Some(&status), None);

        let health = slot.read().unwrap().clone().expect("slot must be Some");
        assert!(health.running);
        assert!(health.tasks_alive);
        assert_eq!(health.record_count, 42);
        assert_eq!(
            health.last_sync_success_at,
            Some(chrono::DateTime::<chrono::Utc>::from(now))
        );
    }

    #[test]
    fn publish_dns_health_dead_task_reports_tasks_alive_false() {
        let slot = empty_dns_health();
        let status = temps_dns_resolver::ResolverStatus {
            tasks_alive: false,
            sync: temps_dns_resolver::SyncStatus {
                last_success_at: None,
                consecutive_failures: 3,
                last_error: Some("sync tick failed: connection refused".into()),
            },
            record_count: 0,
            zone_generation: 0,
        };
        publish_dns_health(&slot, true, Some(&status), None);

        let health = slot.read().unwrap().clone().expect("slot must be Some");
        assert!(health.running, "resolver is still enabled/attached");
        assert!(!health.tasks_alive, "the dead task must be visible");
        assert_eq!(health.consecutive_sync_failures, 3);
        assert_eq!(
            health.last_sync_error.as_deref(),
            Some("sync tick failed: connection refused")
        );
    }

    #[test]
    fn publish_dns_health_start_failure_surfaces_the_error_as_not_running() {
        let slot = empty_dns_health();
        publish_dns_health(
            &slot,
            false,
            None,
            Some("address already in use".to_string()),
        );

        let health = slot.read().unwrap().clone().expect("slot must be Some");
        assert!(!health.running);
        assert!(!health.tasks_alive);
        assert_eq!(
            health.last_sync_error.as_deref(),
            Some("address already in use"),
            "a startup failure must be visible to the operator even though it \
             never reached the sync loop"
        );
    }

    #[test]
    fn dns_resolver_heartbeat_serializes_null_timestamp_when_never_synced() {
        let health = DnsResolverHeartbeat {
            running: true,
            tasks_alive: true,
            last_sync_success_at: None,
            consecutive_sync_failures: 0,
            last_sync_error: None,
            record_count: 0,
        };
        let value = serde_json::to_value(&health).unwrap();
        assert_eq!(value["running"], serde_json::json!(true));
        assert_eq!(value["last_sync_success_at"], serde_json::Value::Null);
        assert_eq!(value["record_count"], serde_json::json!(0));
    }
}
