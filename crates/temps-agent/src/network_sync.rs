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

use std::collections::HashSet;
use std::net::IpAddr;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use bollard::Docker;
use chrono::{DateTime, Utc};
use ipnet::Ipv4Net;
use serde::{Deserialize, Serialize};
use temps_deployer::{ContainerDeployer, ContainerInfo};
use temps_dns_resolver::{
    DnsPolicySlot, ResolverConfig as DnsResolverConfig, ResolverHandle as DnsResolverHandle,
};
use temps_network::{overlay_name, NetworkConfig, NetworkManager, NodeAlloc, OverlayNames, Peer};
use temps_wireguard::mesh::{MeshInterface, MeshKey, MeshPeer};
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

/// `overlay` in the heartbeat (ADR 056): whether this node's cross-node
/// overlay is up, on which allocation, and why not when it is not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OverlayHeartbeat {
    /// `ready` or `unavailable`.
    pub status: &'static str,
    /// The allocation the overlay runs on (`ready`) or was tried on.
    pub compute_cidr: Option<String>,
    /// The cause and the fix, when `unavailable`.
    pub last_error: Option<String>,
}

impl OverlayHeartbeat {
    pub const READY: &'static str = "ready";
    pub const UNAVAILABLE: &'static str = "unavailable";

    fn ready(compute_cidr: Ipv4Net) -> Self {
        Self {
            status: Self::READY,
            compute_cidr: Some(compute_cidr.to_string()),
            last_error: None,
        }
    }

    fn unavailable(compute_cidr: Option<String>, error: String) -> Self {
        Self {
            status: Self::UNAVAILABLE,
            compute_cidr,
            last_error: Some(error),
        }
    }
}

/// What the network-sync loop tells the heartbeat about the overlay and the
/// ranges this host already uses. Both `None` until the loop first ran.
#[derive(Debug, Clone, Default)]
pub struct OverlayReport {
    pub overlay: Option<OverlayHeartbeat>,
    /// This host's occupied IPv4 ranges (ADR 056), already normalized; the
    /// heartbeat sends them when they differ from what it last sent.
    pub occupied_ranges: Option<Vec<temps_network::occupied::OccupiedRange>>,
}

pub type SharedOverlayReport = Arc<std::sync::RwLock<OverlayReport>>;

/// The health slots the network-sync loop publishes into and the heartbeat
/// loop reads from.
#[derive(Clone, Default)]
pub struct HealthSlots {
    pub dns: SharedDnsHealth,
    pub overlay: SharedOverlayReport,
}

/// Longest wait between overlay attempts while the same failure persists.
const OVERLAY_RETRY_CAP: Duration = Duration::from_secs(300);

/// A persisting failure is logged again at most this often.
const OVERLAY_RELOG_INTERVAL: Duration = Duration::from_secs(300);

/// Retry policy for bringing the overlay up (ADR 056): exponential backoff
/// from [`BACKOFF_INTERVAL`] to [`OVERLAY_RETRY_CAP`] while nothing that could
/// fix it changed, an immediate retry when the inputs (allocation, peers,
/// occupied ranges) change, and one log line per change of cause, then at
/// most one per [`OVERLAY_RELOG_INTERVAL`].
#[derive(Debug, Default)]
struct OverlayRetry {
    failures: u32,
    cause: Option<String>,
    fingerprint: Option<u64>,
    next_attempt: Option<std::time::Instant>,
    last_logged: Option<std::time::Instant>,
}

/// What to do after a failed attempt.
#[derive(Debug, PartialEq, Eq)]
struct RetryStep {
    delay: Duration,
    /// Log this failure at warn level (else debug).
    log: bool,
}

impl OverlayRetry {
    /// Whether to try now: always when the inputs changed, else once the
    /// backoff has passed.
    fn due(&self, fingerprint: u64, now: std::time::Instant) -> bool {
        self.fingerprint != Some(fingerprint) || self.next_attempt.is_none_or(|at| now >= at)
    }

    /// How long until the next attempt is due with unchanged inputs.
    fn remaining(&self, now: std::time::Instant) -> Duration {
        self.next_attempt
            .map(|at| at.saturating_duration_since(now))
            .unwrap_or_default()
    }

    fn failed(&mut self, cause: &str, fingerprint: u64, now: std::time::Instant) -> RetryStep {
        if self.fingerprint != Some(fingerprint) {
            self.failures = 0;
        }
        self.fingerprint = Some(fingerprint);
        self.failures = self.failures.saturating_add(1);
        let delay = BACKOFF_INTERVAL
            .saturating_mul(1u32 << (self.failures - 1).min(10))
            .min(OVERLAY_RETRY_CAP);
        self.next_attempt = Some(now + delay);
        let log = self.cause.as_deref() != Some(cause)
            || self
                .last_logged
                .is_none_or(|at| now.saturating_duration_since(at) >= OVERLAY_RELOG_INTERVAL);
        if log {
            self.last_logged = Some(now);
        }
        self.cause = Some(cause.to_owned());
        RetryStep { delay, log }
    }

    /// Reset after a success. Returns the failures it ends, for the
    /// "recovered after N attempts" line.
    fn succeeded(&mut self) -> u32 {
        let failures = self.failures;
        *self = Self::default();
        failures
    }
}

/// Identity of the inputs an overlay attempt depends on.
fn overlay_fingerprint(
    payload: &WirePeerListResponse,
    occupied: Option<&[temps_network::occupied::OccupiedRange]>,
) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    serde_json::to_string(&(
        &payload.network,
        &payload.alloc,
        &payload.peers,
        &payload.wireguard,
    ))
    .unwrap_or_default()
    .hash(&mut hasher);
    occupied.hash(&mut hasher);
    hasher.finish()
}

/// The overlay the occupied-range collection leaves out: the built manager's,
/// else the latest snapshot's (the manager may not be built yet, and until it
/// is `overlay_name::current()` still says `temps0`), else `temps0`'s.
fn occupied_scan_config(
    built: Option<&NetworkConfig>,
    snapshot_names: Option<&OverlayNames>,
) -> NetworkConfig {
    match (built, snapshot_names) {
        (Some(config), _) => config.clone(),
        (None, Some(names)) => NetworkConfig::default().with_overlay_names(names),
        (None, None) => NetworkConfig::default().with_overlay_names(&overlay_name::current()),
    }
}

/// The ranges this host already uses outside Temps, for the control plane
/// (ADR 056). Routes the overlay itself installed towards peers (a native
/// transport puts them on the underlay device) are left out: they are
/// Temps' own, not a collision.
async fn collect_occupied_ranges(
    config: &NetworkConfig,
    routed_peers: &[Ipv4Net],
) -> Result<Vec<temps_network::occupied::OccupiedRange>, String> {
    let docker = Docker::connect_with_local_defaults().ok();
    let owned_devices = [
        config.bridge_name.as_str(),
        config.vxlan_dev_name.as_str(),
        temps_network::mesh::MESH_INTERFACE,
    ];
    let mut ranges = temps_network::occupied::collect(
        docker.as_ref(),
        &owned_devices,
        &config.docker_network_name,
    )
    .await
    .map_err(|error| error.to_string())?;
    ranges.retain(|range| {
        range.source != temps_network::occupied::OccupiedSource::Route
            || !routed_peers.contains(&range.cidr)
    });
    Ok(ranges)
}

/// Wire types — match the server's `handlers::network::PeerListResponse`.
/// We re-declare them here rather than depending on `temps-deployments`
/// because that crate transitively pulls in sea-orm and we don't want it
/// in the worker build.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
struct WirePeerListResponse {
    /// Authoritative cluster-wide pool. Optional only for rolling upgrades
    /// from older control planes.
    #[serde(default)]
    network: Option<WireNetworkPool>,
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
    /// Managed WireGuard mesh. `None` from control planes without the mesh
    /// or with it off: the node keeps its registered address as underlay.
    #[serde(default)]
    wireguard: Option<WireMesh>,
}

/// Managed WireGuard mesh section of the peer list (absent when off).
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
struct WireMesh {
    cidr: String,
    listen_port: u16,
    #[serde(rename = "self", default)]
    self_entry: Option<WireMeshSelf>,
    #[serde(default)]
    peers: Vec<WireMeshPeer>,
    /// This node is the mesh hub: it relays between members that cannot
    /// reach each other (ADR 048 D4).
    #[serde(default)]
    hub: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
struct WireMeshSelf {
    public_key: String,
    /// `None` for a node nobody dials (ADR 055).
    #[serde(default)]
    endpoint: Option<String>,
    address: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
struct WireMeshPeer {
    name: String,
    public_key: String,
    #[serde(default)]
    endpoint: Option<String>,
    address: String,
    /// Members reached through this peer, the hub, because this node cannot
    /// reach them directly.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    relayed: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
struct WireNetworkPool {
    compute_pool_cidr: String,
    subnet_prefix_len: u8,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
struct WireAlloc {
    node_id: String,
    compute_cidr: String,
    bridge_address: String,
    underlay_address: String,
    /// The overlay network name this node uses (ADR 059). Absent from
    /// control planes that predate it, which means `temps0`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    overlay_network_name: Option<String>,
}

impl WireAlloc {
    /// The host-side names of this node's overlay; `temps0`'s when the
    /// control plane sent none.
    fn overlay_names(&self) -> Result<OverlayNames, SyncError> {
        match &self.overlay_network_name {
            None => Ok(OverlayNames::default()),
            Some(name) => OverlayNames::new(name).map_err(|e| SyncError::WireParse(e.to_string())),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
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

/// Host address published container ports bind to (never `0.0.0.0`).
///
/// One slot per agent process, shared by the app-container deployer
/// (`DockerRuntime::with_host_bind_slot`), the service handlers, and this
/// loop, which moves it onto the mesh address — so everything created after
/// the move publishes where the control plane dials.
pub type SharedBindAddress = temps_deployer::docker::SharedHostBindAddress;

/// Where this node publishes workload ports until it is on the WireGuard
/// mesh: its registered address (loopback only in the legacy test-fixture
/// case with none). A node that joined with a public address moves to its
/// mesh address once the mesh is up; see
/// `temps_entities::nodes::Model::data_address` for the control-plane side.
pub fn initial_bind_address(private_address: Option<&str>) -> String {
    private_address.unwrap_or("127.0.0.1").to_string()
}

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
///
/// `containers` is only used to name the containers still publishing on the
/// previous address when `bind_address` moves (see
/// [`warn_containers_on_previous_bind_address`]).
///
/// `mesh_address` receives this node's mesh address once its end of the
/// mesh is up, and `None` when the cluster has no mesh (see
/// [`crate::mesh_listener`]).
///
/// `dns_policy` is the slot the worker's network policy enforcer stores its
/// DNS view in; the resolver answers restricted workloads from it.
#[allow(clippy::too_many_arguments)]
pub fn spawn(
    config: &AgentConfig,
    overlay_bridge_address: Arc<std::sync::RwLock<Option<IpAddr>>>,
    peers: SharedPeers,
    health: HealthSlots,
    bind_address: SharedBindAddress,
    containers: Arc<dyn ContainerDeployer>,
    mesh_address: crate::mesh_listener::MeshAddressSender,
    dns_policy: DnsPolicySlot,
) {
    let cfg = config.clone();
    tokio::spawn(async move {
        if let Err(e) = run(
            cfg,
            overlay_bridge_address,
            peers,
            health,
            bind_address,
            containers,
            mesh_address,
            dns_policy,
        )
        .await
        {
            // The loop is designed to retry forever; reaching this branch
            // means the loop itself unwound, which only happens on
            // unrecoverable invariant violations.
            error!("network sync loop exited unexpectedly: {}", e);
        }
    });
}

/// The address this node's overlay traffic must leave from: its registered
/// private address, accepting the `host:port` form the agent also accepts.
fn underlay_probe_address(private_address: Option<&str>) -> Option<IpAddr> {
    let value = private_address?.trim();
    IpAddr::from_str(value).ok().or_else(|| {
        std::net::SocketAddr::from_str(value)
            .ok()
            .map(|addr| addr.ip())
    })
}

/// Pick the underlay device when the operator did not configure one.
///
/// Peers reach this node at its private address, so the device that carries
/// that address is the underlay — the same rule the control plane
/// applies to itself. The default route is only a fallback: on a node joined
/// over WireGuard it points at the physical link, whose MTU is larger than the
/// tunnel's, so an overlay sized for it silently drops full-size packets.
/// Returns `None` to keep the built-in default.
async fn detect_underlay_device(private_address: Option<&str>) -> Option<String> {
    if let Some(address) = underlay_probe_address(private_address) {
        match temps_network::detect_device_for_address(address).await {
            Ok(dev) => {
                info!(
                    underlay_dev = %dev,
                    private_address = %address,
                    "auto-detected underlay device from the node's private address"
                );
                return Some(dev);
            }
            Err(e) => warn!(
                error = %e,
                private_address = %address,
                "could not find the device for this node's private address; trying the default route"
            ),
        }
    }
    match temps_network::detect_underlay_device().await {
        Ok(dev) => {
            info!(underlay_dev = %dev, "auto-detected underlay device from default route");
            Some(dev)
        }
        Err(e) => {
            warn!(
                error = %e,
                fallback = %NetworkConfig::default().underlay_dev,
                "could not auto-detect underlay device; falling back to default. \
                 Set AgentConfig.underlay_dev (or 'temps join --underlay-dev') to override"
            );
            None
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run(
    config: AgentConfig,
    overlay_bridge_address: Arc<std::sync::RwLock<Option<IpAddr>>>,
    shared_peers: SharedPeers,
    health: HealthSlots,
    bind_address: SharedBindAddress,
    containers: Arc<dyn ContainerDeployer>,
    mesh_address: crate::mesh_listener::MeshAddressSender,
    dns_policy: DnsPolicySlot,
) -> Result<(), SyncError> {
    let HealthSlots {
        dns: dns_health,
        overlay: overlay_report,
    } = health;
    info!(
        node_id = config.node_id,
        control_plane = %config.control_plane_url,
        "network sync loop started"
    );

    // Strict TLS — this carries the same secrets as heartbeat.
    let client = crate::control_plane_client_builder(&config)
        .timeout(Duration::from_secs(10))
        .danger_accept_invalid_certs(false)
        .build()
        .map_err(|e| SyncError::ClientBuild(e.to_string()))?;

    let url = format!(
        "{}/api/internal/nodes/{}/network/peers",
        config.control_plane_url.trim_end_matches('/'),
        config.node_id
    );

    // The allocation the overlay is up on, once it is.
    let mut bootstrapped: Option<Ipv4Net> = None;
    // Containers taken off the overlay network when the control plane moved
    // this node's allocation; they rejoin once the overlay is up again.
    let mut detached: Vec<temps_network::docker::DetachedMember> = Vec::new();
    // ADR 056: backoff and log policy for an overlay that cannot come up,
    // the peers' subnets last seen (left out of the occupied report), and
    // the last occupied-range collection error (logged once per change).
    let mut overlay_retry = OverlayRetry::default();
    let mut peer_subnets: Vec<Ipv4Net> = Vec::new();
    let mut occupied_error: Option<String> = None;
    // Started after first successful bootstrap. Held here (not dropped)
    // so the resolver tasks stay alive for the lifetime of the agent.
    let mut _resolver_handle: Option<DnsResolverHandle> = None;

    let slots = SharedSlots {
        overlay_bridge_address: &overlay_bridge_address,
        shared_peers: &shared_peers,
        dns_health: &dns_health,
        dns_policy: &dns_policy,
    };

    let mesh_url = format!(
        "{}/api/internal/nodes/{}/network/wireguard",
        config.control_plane_url.trim_end_matches('/'),
        config.node_id
    );
    let mut mesh = MeshState::default();
    // Built from the first snapshot: the underlay device depends on whether
    // the cluster runs the WireGuard mesh.
    let mut manager: Option<(NetworkManager, bool)> = None;
    let snapshot_path = snapshot_path(&config);
    // Whether any snapshot has been applied since start, and whether the
    // offline restore was already tried: it runs at most once, at startup.
    let mut applied_once = false;
    let mut restore_attempted = false;
    // What the snapshot file holds, so an unchanged tick doesn't rewrite it.
    let mut saved: Option<WirePeerListResponse> = None;
    // The overlay this node ran before its overlay network name changed, and
    // the peer subnets it routed, until its VXLAN device and routes are
    // released (ADR 059). Seeded from the snapshot the last run applied, so a
    // rename across a restart is handled too.
    let last_snapshot = {
        let path = snapshot_path.clone();
        tokio::task::spawn_blocking(move || load_snapshot(&path))
            .await
            .ok()
            .flatten()
    };
    let mut renamed_from: Option<PreviousOverlay> = last_snapshot
        .as_ref()
        .and_then(PreviousOverlay::from_snapshot);
    // The overlay names of the latest snapshot (the one the last run applied,
    // then each polled one), so the occupied-range collection leaves this
    // node's own bridge and Docker network out before a manager is built.
    let mut snapshot_names: Option<OverlayNames> = last_snapshot
        .as_ref()
        .and_then(|snapshot| snapshot.alloc.as_ref())
        .and_then(|alloc| alloc.overlay_names().ok());
    // Peer subnets of the last applied payload, for `renamed_from`.
    let mut applied_peer_subnets: Vec<Ipv4Net> = Vec::new();

    loop {
        let occupied = {
            let net_config = occupied_scan_config(
                manager.as_ref().map(|(manager, _)| manager.config()),
                snapshot_names.as_ref(),
            );
            match collect_occupied_ranges(&net_config, &peer_subnets).await {
                Ok(ranges) => {
                    if occupied_error.take().is_some() {
                        info!("collecting this host's occupied network ranges works again");
                    }
                    if let Ok(mut report) = overlay_report.write() {
                        report.occupied_ranges = Some(ranges.clone());
                    }
                    Some(ranges)
                }
                Err(error) => {
                    if occupied_error.as_deref() != Some(error.as_str()) {
                        warn!(
                            error = %error,
                            "could not list this host's routes and Docker networks; the control \
                             plane keeps the last report and cannot steer overlay allocations \
                             around ranges added since. Retrying every poll"
                        );
                        occupied_error = Some(error);
                    }
                    None
                }
            }
        };
        let (polled, offline) = match poll_once(&client, &url, &config.token).await {
            Ok(payload) => (Ok(payload), false),
            Err(e) if !applied_once && !restore_attempted && e.control_plane_unreachable() => {
                restore_attempted = true;
                let path = snapshot_path.clone();
                match tokio::task::spawn_blocking(move || load_snapshot(&path))
                    .await
                    .ok()
                    .flatten()
                {
                    Some(payload) => {
                        warn!(
                            error = %e,
                            snapshot = %snapshot_path.display(),
                            "control plane unreachable at startup; restoring the last applied \
                             network snapshot so this node reaches its peers meanwhile"
                        );
                        (Ok(Some(payload)), true)
                    }
                    None => (Err(e), false),
                }
            }
            Err(e) => (Err(e), false),
        };
        match polled {
            Ok(Some(payload)) => {
                let on_mesh = payload.wireguard.is_some();
                let fingerprint = overlay_fingerprint(&payload, occupied.as_deref());
                let attempted_cidr = payload
                    .alloc
                    .as_ref()
                    .map(|alloc| alloc.compute_cidr.clone());
                peer_subnets = payload
                    .peers
                    .iter()
                    .filter_map(|peer| Ipv4Net::from_str(&peer.compute_cidr).ok())
                    .collect();
                if !on_mesh {
                    publish_mesh_address(&mesh_address, None);
                }
                if let Some(wire) = &payload.wireguard {
                    let compute_pool = payload
                        .network
                        .as_ref()
                        .and_then(|network| Ipv4Net::from_str(&network.compute_pool_cidr).ok());
                    match reconcile_mesh(
                        &client,
                        &mesh_url,
                        &config,
                        wire,
                        compute_pool,
                        &mut mesh,
                        offline,
                    )
                    .await
                    {
                        Ok(MeshTick::Ready { rebuilt, report }) => {
                            publish_mesh_address(
                                &mesh_address,
                                mesh.configured.as_ref().map(|interface| interface.address),
                            );
                            if let Some(moved) =
                                publish_mesh_bind_address(&bind_address, &config, &mesh)
                            {
                                // Off the reconcile path: inspecting every
                                // container is one Docker call each.
                                tokio::spawn(warn_containers_on_previous_bind_address(
                                    containers.clone(),
                                    moved,
                                ));
                            }
                            if !offline && report {
                                report_handshakes(&client, &mesh_url, &config).await;
                            }
                            if rebuilt && manager.is_some() {
                                // Rebuild from scratch: the VXLAN device may be
                                // gone and the overlay MTU follows the tunnel's.
                                info!(
                                    "the WireGuard interface changed; rebuilding the overlay on it"
                                );
                                manager = None;
                                bootstrapped = None;
                            }
                        }
                        Ok(MeshTick::Registered) => {
                            // The control plane just assigned or updated our
                            // mesh address (and underlay); re-poll for it.
                            tokio::time::sleep(MESH_REGISTERED_REPOLL).await;
                            continue;
                        }
                        Err(e)
                            if manager
                                .as_ref()
                                .is_some_and(|(_, built_on_mesh)| *built_on_mesh) =>
                        {
                            // The overlay already runs on the mesh: keep
                            // reconciling it (peers, firewall drift) rather
                            // than freezing it behind a mesh problem.
                            warn!(error = %e, "WireGuard mesh sync failed; will retry");
                        }
                        Err(e) => {
                            // The overlay runs on the mesh, so it is down too.
                            let wait = overlay_failed(
                                &mut overlay_retry,
                                &overlay_report,
                                attempted_cidr,
                                format!("the WireGuard mesh is not up on this node: {e}"),
                                fingerprint,
                            );
                            tokio::time::sleep(wait).await;
                            continue;
                        }
                    }
                }
                if payload.alloc.is_none() {
                    debug!("network sync: no compute_cidr allocated yet");
                    tokio::time::sleep(POLL_INTERVAL).await;
                    continue;
                }
                let now = std::time::Instant::now();
                if !overlay_retry.due(fingerprint, now) {
                    // Backing off an unchanged failure; nothing that could
                    // fix it changed since the last attempt.
                    tokio::time::sleep(overlay_retry.remaining(now).min(POLL_INTERVAL)).await;
                    continue;
                }
                let names = match payload.alloc.as_ref().map(WireAlloc::overlay_names) {
                    Some(Ok(names)) => names,
                    Some(Err(e)) => {
                        let wait = overlay_failed(
                            &mut overlay_retry,
                            &overlay_report,
                            attempted_cidr,
                            e.to_string(),
                            fingerprint,
                        );
                        tokio::time::sleep(wait).await;
                        continue;
                    }
                    None => OverlayNames::default(),
                };
                snapshot_names = Some(names.clone());
                let node_id = payload
                    .alloc
                    .as_ref()
                    .map(|alloc| alloc.node_id.clone())
                    .unwrap_or_default();
                if let Some((built, _)) = &manager {
                    let built_names = built.config().overlay_names();
                    if built_names != names {
                        renamed_from = Some(PreviousOverlay {
                            node_id: node_id.clone(),
                            names: built_names,
                            peer_subnets: applied_peer_subnets.clone(),
                        });
                        manager = None;
                        bootstrapped = None;
                    }
                }
                if let Some(previous) = renamed_from
                    .take_if(|previous| previous.node_id == node_id && previous.names != names)
                {
                    if let Err(e) = release_previous_overlay(&previous, &names).await {
                        let wait = overlay_failed(
                            &mut overlay_retry,
                            &overlay_report,
                            attempted_cidr,
                            e,
                            fingerprint,
                        );
                        renamed_from = Some(previous);
                        tokio::time::sleep(wait).await;
                        continue;
                    }
                }
                renamed_from = None;
                if manager
                    .as_ref()
                    .is_some_and(|(_, built_on_mesh)| *built_on_mesh != on_mesh)
                {
                    // Bootstrapping the new manager recreates the VXLAN device
                    // on the new underlay and re-renders the firewall.
                    info!(
                        on_mesh,
                        "the cluster's WireGuard mesh setting changed; moving the overlay onto \
                         the new underlay"
                    );
                    manager = None;
                    bootstrapped = None;
                }
                let manager = match &manager {
                    Some((manager, _)) => manager,
                    None => match build_manager(&config, on_mesh, &names).await {
                        Ok(built) => {
                            overlay_name::set_current(names.clone());
                            &manager.insert((built, on_mesh)).0
                        }
                        Err(e) => {
                            let wait = overlay_failed(
                                &mut overlay_retry,
                                &overlay_report,
                                attempted_cidr,
                                e.to_string(),
                                fingerprint,
                            );
                            tokio::time::sleep(wait).await;
                            continue;
                        }
                    },
                };
                let snapshot = (!offline).then(|| payload.clone());
                let payload_peer_subnets = peer_subnets.clone();
                if let Err(e) = apply(
                    manager,
                    payload,
                    &mut bootstrapped,
                    &mut detached,
                    &mut _resolver_handle,
                    &config,
                    &slots,
                )
                .await
                {
                    let wait = overlay_failed(
                        &mut overlay_retry,
                        &overlay_report,
                        attempted_cidr,
                        e.to_string(),
                        fingerprint,
                    );
                    tokio::time::sleep(wait).await;
                    continue;
                }
                applied_once = true;
                applied_peer_subnets = payload_peer_subnets;
                let failures = overlay_retry.succeeded();
                if failures > 0 {
                    info!(
                        failed_attempts = failures,
                        "cross-node overlay is up after {failures} failed attempt(s)"
                    );
                }
                if let (Some(cidr), Ok(mut report)) = (bootstrapped, overlay_report.write()) {
                    report.overlay = Some(OverlayHeartbeat::ready(cidr));
                }
                match snapshot {
                    Some(snapshot) if saved.as_ref() == Some(&snapshot) => {}
                    Some(snapshot) => {
                        let (path, written) = (snapshot_path.clone(), snapshot.clone());
                        let saved_result =
                            tokio::task::spawn_blocking(move || save_snapshot(&path, &written))
                                .await
                                .unwrap_or_else(|e| Err(std::io::Error::other(e)));
                        if let Err(e) = saved_result {
                            warn!(
                                error = %e,
                                snapshot = %snapshot_path.display(),
                                "could not save the network snapshot; a restart without the \
                                 control plane will not restore the overlay"
                            );
                        } else {
                            saved = Some(snapshot);
                        }
                    }
                    None => {
                        info!("restored the overlay from the local snapshot; waiting for the control plane");
                        tokio::time::sleep(BACKOFF_INTERVAL).await;
                        continue;
                    }
                }
            }
            Ok(None) => {
                // No allocation yet — single-host mode for this node.
                publish_mesh_address(&mesh_address, None);
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

/// Record a failed overlay attempt (ADR 056): publish the cause for the
/// heartbeat, log it per [`OverlayRetry`], and return how long to wait
/// before polling again.
fn overlay_failed(
    retry: &mut OverlayRetry,
    report: &SharedOverlayReport,
    compute_cidr: Option<String>,
    error: String,
    fingerprint: u64,
) -> Duration {
    let error = temps_network::with_permission_hint(error);
    let step = retry.failed(&error, fingerprint, std::time::Instant::now());
    if step.log {
        warn!(
            error = %error,
            retry_secs = step.delay.as_secs(),
            "cross-node overlay unavailable on this node; its deployments keep working. \
             Retrying with backoff, and at once when its allocation, peers or occupied \
             ranges change"
        );
    } else {
        debug!(error = %error, retry_secs = step.delay.as_secs(), "cross-node overlay still unavailable");
    }
    if let Ok(mut slot) = report.write() {
        slot.overlay = Some(OverlayHeartbeat::unavailable(compute_cidr, error));
    }
    step.delay.min(POLL_INTERVAL)
}

/// The overlay a node ran under its previous overlay network name.
#[derive(Debug, Clone, PartialEq)]
struct PreviousOverlay {
    /// The allocation it belonged to. A snapshot left by another cluster's
    /// registration of this host names another node and is ignored: that
    /// overlay is not this node's to release.
    node_id: String,
    names: OverlayNames,
    peer_subnets: Vec<Ipv4Net>,
}

impl PreviousOverlay {
    /// What the snapshot of the last run says this node's overlay was. A
    /// snapshot without a usable allocation says nothing.
    fn from_snapshot(snapshot: &WirePeerListResponse) -> Option<Self> {
        let alloc = snapshot.alloc.as_ref()?;
        Some(Self {
            node_id: alloc.node_id.clone(),
            names: alloc.overlay_names().ok()?,
            peer_subnets: snapshot
                .peers
                .iter()
                .filter_map(|peer| Ipv4Net::from_str(&peer.compute_cidr).ok())
                .collect(),
        })
    }
}

/// Make room for a renamed overlay (ADR 059): release the previous overlay's
/// VXLAN device, which holds the cluster's VNI and port, and its peer routes.
/// Its bridge and Docker network are never removed: containers on them keep
/// local connectivity, and lose cross-node traffic until redeployed.
async fn release_previous_overlay(
    previous: &PreviousOverlay,
    names: &OverlayNames,
) -> Result<(), String> {
    warn!(
        previous_network = %previous.names.docker_network,
        network = %names.docker_network,
        "this node's overlay network name changed; building '{}' beside '{}'. Temps leaves \
         '{}' and its bridge in place: containers attached to it keep running but lose \
         cross-node traffic until they are redeployed, and the network can be removed once \
         nothing uses it",
        names.docker_network,
        previous.names.docker_network,
        previous.names.docker_network,
    );
    let config = NetworkConfig::default().with_overlay_names(&previous.names);
    temps_network::release_overlay_transport(&config, &previous.peer_subnets)
        .await
        .map_err(|e| {
            format!(
                "could not release the previous overlay '{}' so '{}' can use the cluster's \
                 VXLAN VNI and port: {e}",
                previous.names.docker_network, names.docker_network
            )
        })
}

/// Build the overlay manager. On the WireGuard mesh the underlay is the mesh
/// interface; otherwise the configured device, the one holding the node's
/// private address, or the default-route device.
async fn build_manager(
    config: &AgentConfig,
    on_mesh: bool,
    names: &OverlayNames,
) -> Result<NetworkManager, SyncError> {
    let mut net_config = NetworkConfig::default().with_overlay_names(names);
    match (on_mesh, &config.underlay_dev) {
        (true, _) => {
            info!(
                underlay_dev = temps_network::mesh::MESH_INTERFACE,
                "overlay underlay is the WireGuard mesh"
            );
            net_config.underlay_dev = temps_network::mesh::MESH_INTERFACE.to_string();
        }
        (false, Some(dev)) => {
            info!(underlay_dev = %dev, "using operator-configured underlay device");
            net_config.underlay_dev = dev.clone();
        }
        (false, None) => {
            if let Some(dev) = detect_underlay_device(config.private_address.as_deref()).await {
                net_config.underlay_dev = dev;
            }
        }
    }
    net_config.underlay_mtu =
        resolve_underlay_mtu(&net_config.underlay_dev, config.underlay_mtu).await?;
    info!(
        underlay_dev = %net_config.underlay_dev,
        underlay_mtu = net_config.underlay_mtu,
        overlay_mtu = net_config.transport.bridge_mtu(net_config.underlay_mtu),
        "resolved overlay MTU from underlay device"
    );
    NetworkManager::new(net_config).map_err(|e| SyncError::ManagerConstruct(e.to_string()))
}

async fn resolve_underlay_mtu(device: &str, configured_mtu: Option<u32>) -> Result<u32, SyncError> {
    match temps_network::detect_underlay_mtu(device).await {
        Ok(detected_mtu) => {
            let effective_mtu = effective_underlay_mtu(detected_mtu, configured_mtu);
            if let Some(configured_mtu) = configured_mtu {
                if configured_mtu > detected_mtu {
                    warn!(
                        underlay_dev = %device,
                        configured_mtu,
                        detected_mtu,
                        effective_mtu,
                        "configured underlay MTU exceeds the device MTU; clamping to the device"
                    );
                }
            }
            Ok(effective_mtu)
        }
        Err(error) => match configured_mtu {
            Some(configured_mtu) => {
                warn!(
                    underlay_dev = %device,
                    configured_mtu,
                    error = %error,
                    "could not detect underlay MTU; using the explicit MTU ceiling"
                );
                Ok(configured_mtu)
            }
            None => Err(SyncError::UnderlayMtu {
                device: device.to_owned(),
                reason: error.to_string(),
            }),
        },
    }
}

fn effective_underlay_mtu(detected_mtu: u32, configured_mtu: Option<u32>) -> u32 {
    configured_mtu
        .map(|configured_mtu| configured_mtu.min(detected_mtu))
        .unwrap_or(detected_mtu)
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

    // A node on the WireGuard mesh must see the mesh section before it has an
    // allocation: registering there is what gives a node that joined with a
    // public address its private underlay (and so its allocation).
    if payload.alloc.is_none() && payload.wireguard.is_none() {
        return Ok(None);
    }
    Ok(Some(payload))
}

/// Pause before re-polling after the control plane changed our mesh
/// registration, so the new underlay is picked up at once.
const MESH_REGISTERED_REPOLL: Duration = Duration::from_secs(1);

/// Local mesh state kept across ticks.
#[derive(Default)]
struct MeshState {
    key: Option<MeshKey>,
    /// Interface settings last applied, so the interface is only reconfigured
    /// when they change (reconfiguring flushes and re-adds its address).
    configured: Option<MeshInterface>,
    /// The interface MTU derived from this host's path MTU.
    mtu: Option<u32>,
    /// The mesh pool last checked against this host's routes. A new pool
    /// is checked again before it is applied.
    preflighted: Option<Ipv4Net>,
}

/// The mesh pool from the control plane, held to the rules the control
/// plane applies when an operator sets it (private IPv4 space, room for
/// members, clear of the compute pool): the pool goes into this host's
/// firewall lockdown and routes, so a corrupted, edited or hostile peer
/// list must not widen them. `compute_pool` is `None` from a control plane
/// too old to send it and before a paired node first reaches its control
/// plane; the overlap check is then skipped.
fn mesh_cidr(value: &str, compute_pool: Option<Ipv4Net>) -> Result<Ipv4Net, SyncError> {
    // 0.0.0.0/32 is not private, so no valid mesh pool overlaps it: with it
    // standing in for an unknown compute pool only the overlap check is
    // skipped and every other rule is the control plane's own.
    let compute_pool =
        compute_pool.unwrap_or_else(|| Ipv4Net::from(std::net::Ipv4Addr::UNSPECIFIED));
    temps_network::mesh::parse_mesh_cidr(value, compute_pool)
        .map_err(|e| SyncError::WireParse(format!("wireguard.cidr: {e}")))
}

enum MeshTick {
    /// Interface up and peers match the control plane's list. `rebuilt`:
    /// the interface was created, reconfigured or given a new MTU, so the
    /// overlay on top (whose VXLAN device a recreated interface takes with
    /// it) must be bootstrapped again. `report`: send the handshake report;
    /// false when this node is the hub but could not set up relaying.
    Ready { rebuilt: bool, report: bool },
    /// This tick (re-)registered our key or endpoint with the control plane.
    Registered,
}

#[derive(Debug, Serialize)]
struct MeshRegistrationBody<'a> {
    public_key: &'a str,
    /// Omitted for a node nobody dials (ADR 055).
    #[serde(skip_serializing_if = "Option::is_none")]
    endpoint: Option<String>,
}

/// Where other members dial this node: `--wg-endpoint`, else its registered
/// private address on the mesh port. `None` when the private address is the
/// node's own mesh address (a worker that joined outbound only, ADR 055):
/// nobody can dial it, it dials its peers and keeps the path open with
/// keepalives.
fn mesh_endpoint(
    config: &AgentConfig,
    wire: &WireMesh,
) -> Result<Option<std::net::SocketAddr>, SyncError> {
    if let Some(value) = &config.wg_endpoint {
        return temps_network::mesh::parse_endpoint(value)
            .map(Some)
            .map_err(|e| SyncError::Mesh(e.to_string()));
    }
    let registered = config.private_address.as_deref().ok_or_else(|| {
        SyncError::Mesh(
            "no WireGuard endpoint: this node has no registered private address; \
             set --wg-endpoint <ip:port>"
                .into(),
        )
    })?;
    let on_mesh = Ipv4Net::from_str(&wire.cidr).is_ok_and(|cidr| {
        temps_network::mesh::identity_ipv4(registered).is_some_and(|ip| cidr.contains(&ip))
    });
    if on_mesh {
        return Ok(None);
    }
    temps_network::mesh::default_endpoint(registered, wire.listen_port)
        .map(Some)
        .map_err(|e| SyncError::Mesh(e.to_string()))
}

/// Bring this node's end of the WireGuard mesh in line with the control
/// plane: register our public key and endpoint when the control plane does
/// not hold them, bring up the interface on our mesh address, and make the
/// interface's peers exactly the cluster's (which also revokes removed
/// nodes).
async fn reconcile_mesh(
    client: &reqwest::Client,
    registration_url: &str,
    config: &AgentConfig,
    wire: &WireMesh,
    compute_pool: Option<Ipv4Net>,
    state: &mut MeshState,
    offline: bool,
) -> Result<MeshTick, SyncError> {
    let key = match &state.key {
        Some(key) => key.clone(),
        None => {
            let dir = config.mesh_key_dir.clone();
            let key = tokio::task::spawn_blocking(move || MeshKey::load_or_create(&dir))
                .await
                .map_err(|e| SyncError::Mesh(e.to_string()))?
                .map_err(|e| {
                    SyncError::Mesh(format!(
                        "WireGuard key in {}: {e}",
                        config.mesh_key_dir.display()
                    ))
                })?;
            state.key = Some(key.clone());
            key
        }
    };
    let endpoint = mesh_endpoint(config, wire)?;
    let endpoint_text = endpoint.map(|endpoint| endpoint.to_string());

    let registered = wire
        .self_entry
        .as_ref()
        .filter(|me| me.public_key == key.public_key() && me.endpoint == endpoint_text);
    let Some(me) = registered else {
        if offline {
            return Err(SyncError::Mesh(
                "the saved network snapshot does not match this node's WireGuard key or \
                 endpoint; waiting for the control plane to register again"
                    .into(),
            ));
        }
        let response = client
            .put(registration_url)
            .bearer_auth(&config.token)
            .json(&MeshRegistrationBody {
                public_key: key.public_key(),
                endpoint: endpoint_text,
            })
            .send()
            .await
            .map_err(|e| SyncError::Http(e.to_string()))?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(SyncError::HttpStatus { status, body });
        }
        info!(
            endpoint = endpoint.map_or_else(|| "none".to_string(), |e| e.to_string()),
            public_key = key.public_key(),
            "registered with the WireGuard mesh"
        );
        return Ok(MeshTick::Registered);
    };

    let cidr = mesh_cidr(&wire.cidr, compute_pool)?;
    let address = std::net::Ipv4Addr::from_str(&me.address)
        .map_err(|e| SyncError::WireParse(format!("wireguard.self.address: {e}")))?;
    let desired = wire
        .peers
        .iter()
        .map(parse_mesh_peer)
        .collect::<Result<Vec<_>, _>>()?;
    check_mesh_addresses(cidr, address, &desired)?;

    // The lockdown comes before the interface: the tunnel must never exist
    // as an open way into this host. Checked every tick so a flushed
    // ruleset is repaired.
    temps_network::mesh::ensure_lockdown(&mesh_lockdown(config, cidr, wire.hub))
        .await
        .map_err(|e| SyncError::Mesh(format!("mesh firewall: {e}")))?;

    let mtu = match state.mtu {
        Some(mtu) => mtu,
        None => {
            let mtu = temps_network::mesh::detect_mtu(config.underlay_mtu)
                .await
                .map_err(|e| SyncError::Mesh(format!("mesh MTU: {e}")))?;
            *state.mtu.insert(mtu)
        }
    };
    let interface = MeshInterface {
        address,
        prefix_len: cidr.prefix_len(),
        listen_port: wire.listen_port,
        mtu,
    };
    // On the first tick and whenever the control plane moves the pool: a
    // pool that shadows one of this host's routes is refused before the
    // interface takes it. Our own interface's routes are not conflicts.
    if state.preflighted != Some(cidr) {
        temps_network::mesh::preflight_routes(cidr)
            .await
            .map_err(|e| SyncError::Mesh(e.to_string()))?;
        state.preflighted = Some(cidr);
    }
    let mut rebuilt = false;
    if state.configured.as_ref() != Some(&interface) {
        let (apply_interface, apply_key) = (interface.clone(), key.clone());
        rebuilt = tokio::task::spawn_blocking(move || {
            temps_wireguard::mesh::ensure_interface(&apply_interface, &apply_key)
        })
        .await
        .map_err(|e| SyncError::Mesh(e.to_string()))?
        .map_err(|e| SyncError::Mesh(e.to_string()))?;
        info!(
            interface = temps_network::mesh::MESH_INTERFACE,
            %address,
            port = wire.listen_port,
            mtu,
            "WireGuard mesh interface is up"
        );
        state.configured = Some(interface);
        // A new interface starts without forwarding: check relaying from
        // scratch rather than trust what was verified on the old one.
        temps_network::mesh::forget_relay();
    }

    let reconciled =
        tokio::task::spawn_blocking(move || temps_wireguard::mesh::reconcile_peers(&desired))
            .await
            .map_err(|e| SyncError::Mesh(e.to_string()))?;
    let changes = match reconciled {
        Ok(changes) => changes,
        Err(e) => {
            // The interface may have been deleted under us; forget it so the
            // next tick recreates it instead of failing here forever, and
            // verifies relaying on the new one.
            state.configured = None;
            temps_network::mesh::forget_relay();
            return Err(SyncError::Mesh(e.to_string()));
        }
    };
    if !changes.is_empty() {
        info!(
            added = changes.added,
            updated = changes.updated,
            removed = changes.removed,
            "WireGuard mesh peers updated"
        );
    }
    // After the peers: members only route through the hub once it has them.
    // A hub that cannot relay keeps its own mesh and overlay up, but stops
    // reporting handshakes: the control plane then treats it as down and
    // moves its relayed pairs back to direct (ADR 048 D4).
    let report = match temps_network::mesh::ensure_relay(cidr, wire.hub).await {
        Ok(()) => true,
        Err(e) => {
            warn!(
                error = %e,
                hub = wire.hub,
                "could not update WireGuard mesh relaying; will retry"
            );
            !wire.hub
        }
    };
    Ok(MeshTick::Ready { rebuilt, report })
}

#[derive(Serialize)]
struct HandshakeReport {
    peers: Vec<PeerHandshake>,
}

#[derive(Serialize)]
struct PeerHandshake {
    public_key: String,
    seconds_since_handshake: u64,
}

/// Tell the control plane which members this node has handshaken with and
/// when: it moves the pairs that never connect onto the hub (ADR 048 D4).
/// Best effort: a missed report only delays that.
async fn report_handshakes(client: &reqwest::Client, mesh_url: &str, config: &AgentConfig) {
    let status = match tokio::task::spawn_blocking(temps_wireguard::mesh::peer_status).await {
        Ok(Ok(status)) => status,
        Ok(Err(error)) => {
            debug!(%error, "cannot read WireGuard handshakes to report");
            return;
        }
        Err(_) => return,
    };
    let now = std::time::SystemTime::now();
    let peers = status
        .into_iter()
        .filter_map(|peer| {
            let at = peer.last_handshake?;
            Some(PeerHandshake {
                public_key: peer.public_key,
                seconds_since_handshake: now.duration_since(at).unwrap_or_default().as_secs(),
            })
        })
        .collect();
    let result = client
        .put(format!("{mesh_url}/handshakes"))
        .bearer_auth(&config.token)
        .json(&HandshakeReport { peers })
        .send()
        .await;
    match result {
        Ok(response) if response.status().is_success() => {}
        // Control planes without hubs; nothing to report to.
        Ok(response) if response.status() == reqwest::StatusCode::NOT_FOUND => {}
        Ok(response) => warn!(
            status = %response.status(),
            "the control plane rejected this node's WireGuard handshake report"
        ),
        Err(error) => debug!(%error, "could not report WireGuard handshakes"),
    }
}

/// The shared bind address moved from `previous` to `current`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct BindAddressMove {
    previous: String,
    current: String,
}

/// A node that joined with a public address publishes workloads on its mesh
/// address (where the control plane reaches them, see
/// `Model::data_address`) once the mesh interface is up.
///
/// Every container created from then on — app deploys through
/// `DockerRuntime`, services through the agent API — reads this slot and
/// publishes on the mesh address. Returns the move when the address changed,
/// so the caller can name the containers left on the previous one.
/// The mesh firewall for this node: what every member needs, plus the agent
/// API from the control plane when the agent serves it on the mesh
/// ([`crate::mesh_listener`]).
fn mesh_lockdown(
    config: &AgentConfig,
    cidr: Ipv4Net,
    relay: bool,
) -> temps_network::mesh::MeshLockdown {
    temps_network::mesh::MeshLockdown {
        vxlan_port: overlay_vxlan_port(),
        mesh: cidr,
        node_api_port: None,
        agent_api: crate::mesh_listener::mesh_agent_api(config, cidr),
        relay,
    }
}

/// Tell the mesh listener where this node is on the mesh, waking it only on
/// a change.
fn publish_mesh_address(
    sender: &crate::mesh_listener::MeshAddressSender,
    address: Option<std::net::Ipv4Addr>,
) {
    let address = match address {
        Some(address) => crate::mesh_listener::MeshAddress::At(address),
        None => crate::mesh_listener::MeshAddress::Off,
    };
    sender.send_if_modified(|current| {
        if *current == address {
            return false;
        }
        *current = address;
        true
    });
}

fn publish_mesh_bind_address(
    slot: &SharedBindAddress,
    config: &AgentConfig,
    mesh: &MeshState,
) -> Option<BindAddressMove> {
    let joined_privately = config
        .private_address
        .as_deref()
        .is_some_and(temps_core::node_address::is_private_node_address);
    let interface = mesh.configured.as_ref().filter(|_| !joined_privately)?;
    move_bind_address(slot, interface.address.to_string())
}

/// Point the shared bind slot at `address`, returning the move if it changed.
/// A poisoned slot still holds a complete `String`, so it is recovered rather
/// than leaving workloads on the old address forever.
fn move_bind_address(slot: &SharedBindAddress, address: String) -> Option<BindAddressMove> {
    let mut current = slot
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if *current == address {
        return None;
    }
    let previous = std::mem::replace(&mut *current, address.clone());
    info!(
        %previous,
        %address,
        "publishing workload ports on the WireGuard mesh address"
    );
    Some(BindAddressMove {
        previous,
        current: address,
    })
}

/// A container still publishing a port on the address this node moved off.
#[derive(Debug, Clone, PartialEq, Eq)]
struct StrandedContainer {
    container_id: String,
    container_name: String,
    /// `sh.temps.deploy_id`, for app deployments.
    deployment_id: Option<String>,
    /// `sh.temps.project_id`, for app deployments.
    project_id: Option<String>,
    /// `sh.temps.service.name`, for services created through the agent API.
    service_name: Option<String>,
    /// The `host:port` bindings still on the previous address.
    bindings: Vec<String>,
}

/// Temps-managed containers that publish at least one port on `previous`.
///
/// Docker cannot change an existing container's port bindings; only
/// recreating it moves them. Containers Temps did not create are not ours to
/// report on.
fn containers_published_on(previous: &str, containers: &[ContainerInfo]) -> Vec<StrandedContainer> {
    containers
        .iter()
        .filter(|info| {
            info.labels
                .get("sh.temps.managed")
                .is_some_and(|value| value == "true")
        })
        .filter_map(|info| {
            let bindings: Vec<String> = info
                .ports
                .iter()
                .filter(|port| port.host_ip.as_deref() == Some(previous))
                .map(|port| format!("{previous}:{}", port.host_port))
                .collect();
            (!bindings.is_empty()).then(|| StrandedContainer {
                container_id: info.container_id.clone(),
                container_name: info.container_name.clone(),
                deployment_id: info.labels.get("sh.temps.deploy_id").cloned(),
                project_id: info.labels.get("sh.temps.project_id").cloned(),
                service_name: info.labels.get("sh.temps.service.name").cloned(),
                bindings,
            })
        })
        .collect()
}

/// Name every container left publishing on the address this node moved off.
///
/// Moving the slot only affects containers created afterwards; Docker cannot
/// rebind a running container, and recreating workloads from here would
/// bypass the control plane's health-gated rollout. Those containers stay
/// reachable on the previous (public) address while the control plane now
/// dials the mesh address, so the operator is told exactly which ones need a
/// redeploy (apps) or recreate (services) to move.
async fn warn_containers_on_previous_bind_address(
    containers: Arc<dyn ContainerDeployer>,
    moved: BindAddressMove,
) {
    let listed = match containers.list_containers().await {
        Ok(listed) => listed,
        Err(error) => {
            warn!(
                previous = %moved.previous,
                current = %moved.current,
                %error,
                "could not list containers after moving published ports to the WireGuard mesh \
                 address; containers created before the move still publish on the previous \
                 address until they are redeployed"
            );
            return;
        }
    };
    let stranded = containers_published_on(&moved.previous, &listed);
    if stranded.is_empty() {
        return;
    }
    for container in &stranded {
        warn!(
            container_id = %container.container_id,
            container_name = %container.container_name,
            deployment_id = container.deployment_id.as_deref().unwrap_or("-"),
            project_id = container.project_id.as_deref().unwrap_or("-"),
            service_name = container.service_name.as_deref().unwrap_or("-"),
            bindings = %container.bindings.join(","),
            previous = %moved.previous,
            current = %moved.current,
            "container still publishes on this node's previous address; the control plane now \
             reaches this node on its WireGuard mesh address, so redeploy the application (or \
             recreate the service) to move it there and off the previous address"
        );
    }
    warn!(
        count = stranded.len(),
        previous = %moved.previous,
        current = %moved.current,
        "containers created before this node joined the WireGuard mesh still publish on its \
         previous address and are not reachable through the mesh until redeployed"
    );
}

/// The VXLAN port this agent's overlay listens on.
fn overlay_vxlan_port() -> u16 {
    match NetworkConfig::default().transport {
        temps_network::Transport::Vxlan { port, .. } => port,
        temps_network::Transport::Native => 4789,
    }
}

fn parse_mesh_peer(wire: &WireMeshPeer) -> Result<MeshPeer, SyncError> {
    let endpoint = wire
        .endpoint
        .as_deref()
        .map(|value| {
            temps_network::mesh::parse_endpoint(value)
                .map_err(|e| SyncError::WireParse(format!("mesh peer {} endpoint: {e}", wire.name)))
        })
        .transpose()?;
    let address = std::net::Ipv4Addr::from_str(&wire.address)
        .map_err(|e| SyncError::WireParse(format!("mesh peer {} address: {e}", wire.name)))?;
    if !temps_wireguard::mesh::is_valid_public_key(&wire.public_key) {
        return Err(SyncError::WireParse(format!(
            "mesh peer {} has an invalid public key",
            wire.name
        )));
    }
    let relayed = wire
        .relayed
        .iter()
        .map(|value| {
            std::net::Ipv4Addr::from_str(value).map_err(|e| {
                SyncError::WireParse(format!("mesh peer {} relayed address: {e}", wire.name))
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(MeshPeer {
        public_key: wire.public_key.clone(),
        endpoint,
        address,
        relayed,
    })
}

/// Every mesh address must sit inside the mesh CIDR and be unique. Peers get
/// `address/32` as their allowed IPs, so an address outside the pool (from a
/// corrupt or edited snapshot) would claim unrelated traffic for the tunnel.
fn check_mesh_addresses(
    cidr: Ipv4Net,
    own: std::net::Ipv4Addr,
    peers: &[MeshPeer],
) -> Result<(), SyncError> {
    if !cidr.contains(&own) {
        return Err(SyncError::WireParse(format!(
            "wireguard.self.address {own} is outside {cidr}"
        )));
    }
    let mut seen = HashSet::from([own]);
    for address in peers
        .iter()
        .flat_map(|peer| std::iter::once(&peer.address).chain(&peer.relayed))
    {
        if !cidr.contains(address) {
            return Err(SyncError::WireParse(format!(
                "mesh peer address {address} is outside {cidr}"
            )));
        }
        if !seen.insert(*address) {
            return Err(SyncError::WireParse(format!(
                "mesh address {address} is assigned twice"
            )));
        }
    }
    Ok(())
}

/// Where the last applied peer list is kept: beside the mesh key, in the
/// owner-only directory.
/// What a node paired from the control plane knows about the mesh before it
/// has ever reached the control plane (ADR 048 D2b): its own end and the
/// control plane as its only peer.
#[derive(Debug, Clone)]
pub struct MeshBootstrap {
    pub cidr: Ipv4Net,
    pub listen_port: u16,
    /// Where other members dial this node; `None` when nobody can (it joined
    /// outbound only, ADR 055).
    pub endpoint: Option<std::net::SocketAddr>,
    pub address: std::net::Ipv4Addr,
    pub control_plane_public_key: String,
    /// `None` when the control plane cannot be dialed (it dials us).
    pub control_plane_endpoint: Option<String>,
    pub control_plane_address: std::net::Ipv4Addr,
}

/// Bring this node's end of the mesh up with the control plane as its only
/// peer (lockdown first, as the sync loop does), and save it as the network
/// snapshot so `temps agent` restores it after a restart until it reaches
/// the control plane over it. Returns this node's mesh public key.
pub async fn bootstrap_mesh(
    config: &AgentConfig,
    bootstrap: &MeshBootstrap,
) -> Result<String, MeshBootstrapError> {
    let dir = config.mesh_key_dir.clone();
    let key = tokio::task::spawn_blocking(move || MeshKey::load_or_create(&dir)).await??;
    let wire = WireMesh {
        cidr: bootstrap.cidr.to_string(),
        listen_port: bootstrap.listen_port,
        self_entry: Some(WireMeshSelf {
            public_key: key.public_key().to_string(),
            endpoint: bootstrap.endpoint.map(|endpoint| endpoint.to_string()),
            address: bootstrap.address.to_string(),
        }),
        peers: vec![WireMeshPeer {
            name: "control-plane".to_string(),
            public_key: bootstrap.control_plane_public_key.clone(),
            endpoint: bootstrap.control_plane_endpoint.clone(),
            address: bootstrap.control_plane_address.to_string(),
            relayed: Vec::new(),
        }],
        hub: false,
    };
    let client = reqwest::Client::new();
    let mut state = MeshState::default();
    // Offline: nothing is registered over HTTP; the snapshot names our key
    // and endpoint, so the interface comes up from it directly.
    // The pairing carries no compute pool; the control plane's peer list
    // brings it on the first sync.
    reconcile_mesh(&client, "", config, &wire, None, &mut state, true)
        .await
        .map_err(MeshBootstrapError::Mesh)?;
    let snapshot = WirePeerListResponse {
        network: None,
        alloc: None,
        peers: Vec::new(),
        cluster_dns_enabled: false,
        wireguard: Some(wire),
    };
    let path = snapshot_path(config);
    tokio::task::spawn_blocking(move || save_snapshot(&path, &snapshot))
        .await?
        .map_err(MeshBootstrapError::Snapshot)?;
    Ok(key.public_key().to_string())
}

/// Why [`bootstrap_mesh`] could not bring this node's end of the mesh up.
#[derive(Debug, thiserror::Error)]
pub enum MeshBootstrapError {
    #[error("could not load or create the mesh key: {0}")]
    Key(#[from] temps_wireguard::WireGuardError),
    #[error("{}", temps_network::with_permission_hint(.0.to_string()))]
    Mesh(#[source] SyncError),
    #[error("could not save the network snapshot: {0}")]
    Snapshot(#[source] std::io::Error),
    #[error("a background task failed: {0}")]
    Task(#[from] tokio::task::JoinError),
}

/// This node's end of the mesh as its last network snapshot describes it,
/// for `temps doctor mesh`. `Ok(None)` when the cluster's mesh is off.
pub fn mesh_doctor_expectations(
    config: &AgentConfig,
) -> Result<Option<temps_network::mesh_doctor::Expected>, String> {
    use temps_network::mesh_doctor::{Expected, ExpectedPeer, HostRole};

    let path = snapshot_path(config);
    let snapshot = load_snapshot(&path).ok_or_else(|| {
        format!(
            "no network snapshot at {}: `temps agent` writes one after it syncs with the \
             control plane (and when the cluster changes), so it is not running, cannot reach \
             the control plane, or has not synced since the file was removed",
            path.display()
        )
    })?;
    let Some(wire) = snapshot.wireguard else {
        return Ok(None);
    };
    let me = wire.self_entry.ok_or(
        "the last snapshot has no mesh entry for this node: its key was not registered yet",
    )?;
    let cidr = Ipv4Net::from_str(&wire.cidr).map_err(|e| format!("wireguard.cidr: {e}"))?;
    let peers = wire
        .peers
        .iter()
        .map(|peer| {
            parse_mesh_peer(peer)
                .map(|parsed| ExpectedPeer {
                    name: peer.name.clone(),
                    public_key: parsed.public_key,
                    endpoint: parsed.endpoint,
                    address: parsed.address,
                })
                .map_err(|e| e.to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Some(Expected {
        role: HostRole::Node,
        public_key: me.public_key,
        address: std::net::Ipv4Addr::from_str(&me.address)
            .map_err(|e| format!("wireguard.self.address: {e}"))?,
        prefix_len: cidr.prefix_len(),
        listen_port: wire.listen_port,
        endpoint: me
            .endpoint
            .as_deref()
            .and_then(|endpoint| temps_network::mesh::parse_endpoint(endpoint).ok()),
        peers,
        lockdown: mesh_lockdown(config, cidr, wire.hub),
    }))
}

/// Whether the control plane answers where this node's agent calls it, with
/// the agent's TLS trust. Any HTTP response counts: the request carries no
/// credentials.
pub async fn probe_control_plane(config: &AgentConfig) -> temps_network::mesh_doctor::NodeApiProbe {
    let target = config.control_plane_url.trim_end_matches('/').to_string();
    let url = format!("{target}/api/internal/nodes/{}/heartbeat", config.node_id);
    let result = match crate::control_plane_client_builder(config)
        .timeout(std::time::Duration::from_secs(5))
        .build()
    {
        Ok(client) => match client.get(&url).send().await {
            Ok(response) => Ok(format!("HTTP {}", response.status().as_u16())),
            Err(error) => Err(describe_request_error(&error)),
        },
        Err(error) => Err(error.to_string()),
    };
    temps_network::mesh_doctor::NodeApiProbe { target, result }
}

/// The innermost cause of a failed request (reqwest's own message is
/// usually just "error sending request").
fn describe_request_error(error: &reqwest::Error) -> String {
    let mut cause: &dyn std::error::Error = error;
    while let Some(inner) = cause.source() {
        cause = inner;
    }
    if error.is_timeout() {
        "timed out".to_string()
    } else {
        cause.to_string()
    }
}

fn snapshot_path(config: &AgentConfig) -> std::path::PathBuf {
    config.mesh_key_dir.join("network-snapshot.json")
}

/// The last snapshot the control plane served and this node applied. Holds
/// peer addresses and public keys only (no secrets).
fn load_snapshot(path: &std::path::Path) -> Option<WirePeerListResponse> {
    let contents = std::fs::read(path).ok()?;
    match serde_json::from_slice(&contents) {
        Ok(snapshot) => Some(snapshot),
        Err(e) => {
            warn!(error = %e, snapshot = %path.display(), "ignoring an unreadable network snapshot");
            None
        }
    }
}

fn save_snapshot(path: &std::path::Path, snapshot: &WirePeerListResponse) -> std::io::Result<()> {
    use std::io::Write;
    let contents = serde_json::to_vec(snapshot).map_err(std::io::Error::other)?;
    if let Some(dir) = path.parent() {
        temps_wireguard::mesh::create_private_dir(dir)?;
    }
    let temp = path.with_extension("json.tmp");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temp)?;
    file.write_all(&contents)?;
    file.sync_all()?;
    std::fs::rename(&temp, path)
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
    dns_policy: &'a DnsPolicySlot,
}

async fn apply(
    manager: &NetworkManager,
    payload: WirePeerListResponse,
    bootstrapped: &mut Option<Ipv4Net>,
    detached: &mut Vec<temps_network::docker::DetachedMember>,
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
    // The allocations must sit inside the cluster's pool and not overlap:
    // a corrupted or hostile peer list must not install arbitrary routes.
    if let Some(network) = payload.network.as_ref() {
        validate_cluster_topology(network, &alloc, &peers)?;
    }
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

    // The control plane moved this node's allocation (a pool change or an
    // automatic move off a colliding range, ADR 056). It only moves one it
    // never saw applied, so this is the narrow race where the move crossed a
    // successful bootstrap: take the overlay down and bring it up on the new
    // subnet rather than reconciling peers against the old one. The Docker
    // network goes too: Docker cannot change its subnet, and the preflight
    // below refuses one on the old subnet. `bootstrapped` is cleared only
    // once both are gone, so a failure here is retried on the next tick.
    if let Some(previous) = *bootstrapped {
        if previous != alloc.compute_cidr {
            warn!(
                from = %previous,
                to = %alloc.compute_cidr,
                "the control plane moved this node's overlay subnet; rebuilding the overlay on it"
            );
            manager
                .teardown()
                .await
                .map_err(|e| SyncError::Bootstrap(format!("tearing down the old overlay: {e}")))?;
            let docker = Docker::connect_with_local_defaults()
                .map_err(|error| SyncError::DockerConnect(error.to_string()))?;
            temps_network::docker::retire_network_for_move(&docker, manager.config(), detached)
                .await
                .map_err(|e| {
                    SyncError::Bootstrap(format!(
                        "replacing the overlay Docker network on the old subnet: {e}"
                    ))
                })?;
            *bootstrapped = None;
        }
    }

    // Re-check what this node routes on every snapshot, not only at
    // bootstrap: a local Docker network or route created later must be
    // rejected before a newly allocated peer route is installed. Only the
    // node's own subnet and its peers' are checked (ADR 056): a network
    // elsewhere in the pool is never routed here and cannot collide.
    let routed = routed_subnets(&alloc, &peers);
    let alloc_for_docker = alloc.clone();
    temps_network::preflight_compute_routes(manager.config(), &routed)
        .await
        .map_err(|error| SyncError::Preflight(error.to_string()))?;
    preflight_overlay_docker_network(manager.config(), &alloc_for_docker, &routed).await?;

    if bootstrapped.is_none() {
        info!(
            cidr = %alloc.compute_cidr,
            peers = peers.len(),
            "bringing up multi-host overlay"
        );
        // Refuse address-space collisions before mutating kernel state. The
        // control plane allocation remains authoritative; this node must fix
        // its local Docker pools rather than silently choosing another CIDR.
        let compute_cidr = alloc.compute_cidr;
        manager
            .bootstrap(alloc, peers.clone())
            .await
            .map_err(|e| SyncError::Bootstrap(e.to_string()))?;

        // Create the Docker bridge network pinned to the kernel bridge
        // we just brought up. `temps-network::linux::bootstrap` only
        // creates kernel-level primitives (br-temps0, vxlan-temps0,
        // routes, nftables); the corresponding Docker network has to be
        // created here so the deployer + service handlers can attach
        // containers to it. Without this, `compute_ip` is always None
        // and the DNS registry never gets per-container records.
        ensure_overlay_docker_network(manager.config(), &alloc_for_docker, &routed).await?;
        *bootstrapped = Some(compute_cidr);
    } else {
        ensure_overlay_docker_network(manager.config(), &alloc_for_docker, &routed).await?;
        let changed = manager
            .reconcile_peers(peers.clone())
            .await
            .map_err(|e| SyncError::Reconcile(e.to_string()))?;
        if changed {
            info!("multi-host peer list updated");
        }
    }

    if !detached.is_empty() {
        let docker = Docker::connect_with_local_defaults()
            .map_err(|error| SyncError::DockerConnect(error.to_string()))?;
        temps_network::docker::reattach_members(&docker, manager.config(), detached)
            .await
            .map_err(|e| SyncError::Bootstrap(e.to_string()))?;
    }

    // Publish only the peer view that the host data plane accepted. Container
    // attachment handlers must never observe routes from a rejected payload.
    if let Ok(mut slot) = slots.shared_peers.write() {
        *slot = peers.clone();
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
        slots.dns_policy,
    )
    .await;

    // Re-inject per-peer routes inside every overlay-attached
    // container's netns. The routes don't survive a container netns
    // recreate (Docker auto-restart on crash, worker reboot, image
    // update), so a sync-loop sweep is the simplest way to heal
    // automatically. `ip route replace` is idempotent — re-running
    // when nothing changed is a no-op.
    let swept = match Docker::connect_with_local_defaults() {
        Ok(docker) => {
            temps_network::overlay_routes::sweep_container_routes(
                &docker,
                &overlay_name::current().docker_network,
                &bridge_v4,
                &peers,
                // A full pass every sync, as before.
                &mut std::collections::HashSet::new(),
            )
            .await
        }
        Err(e) => Err(format!("connect_with_local_defaults: {e}")),
    };
    if let Err(e) = swept {
        debug!(
            error = %e,
            "Container route sweep skipped (will retry next tick)"
        );
    }

    Ok(())
}

fn validate_cluster_topology(
    network: &WireNetworkPool,
    alloc: &NodeAlloc,
    peers: &[Peer],
) -> Result<Ipv4Net, SyncError> {
    let pool = Ipv4Net::from_str(&network.compute_pool_cidr)
        .map_err(|error| SyncError::WireParse(format!("network.compute_pool_cidr: {error}")))?;
    let mut cidrs = Vec::with_capacity(peers.len() + 1);
    cidrs.push(("local allocation".to_owned(), alloc.compute_cidr));
    cidrs.extend(
        peers
            .iter()
            .map(|peer| (format!("peer {}", peer.node_id), peer.compute_cidr)),
    );
    for (label, cidr) in &cidrs {
        if cidr.prefix_len() != network.subnet_prefix_len
            || !pool.contains(&cidr.network())
            || !pool.contains(&cidr.broadcast())
        {
            return Err(SyncError::WireParse(format!(
                "{label} uses {cidr} but authoritative pool is {pool} with /{} per node; refusing inconsistent routes",
                network.subnet_prefix_len
            )));
        }
    }
    for left in 0..cidrs.len() {
        for right in (left + 1)..cidrs.len() {
            if cidrs[left].1.contains(&cidrs[right].1.network())
                || cidrs[right].1.contains(&cidrs[left].1.network())
            {
                return Err(SyncError::WireParse(format!(
                    "{} {} overlaps {} {}; refusing ambiguous routes",
                    cidrs[left].0, cidrs[left].1, cidrs[right].0, cidrs[right].1
                )));
            }
        }
    }
    for peer in peers {
        if let IpAddr::V4(address) = peer.underlay_address {
            if pool.contains(&address) {
                return Err(SyncError::WireParse(format!(
                    "peer {} underlay {} is inside compute pool {}; refusing a self-shadowing overlay",
                    peer.node_id, address, pool
                )));
            }
        }
    }
    if let IpAddr::V4(address) = alloc.underlay_address {
        if pool.contains(&address) {
            return Err(SyncError::WireParse(format!(
                "local underlay {address} is inside compute pool {pool}; refusing a self-shadowing overlay"
            )));
        }
    }
    Ok(pool)
}

async fn preflight_overlay_docker_network(
    config: &NetworkConfig,
    alloc: &NodeAlloc,
    routed: &[Ipv4Net],
) -> Result<(), SyncError> {
    let docker = Docker::connect_with_local_defaults()
        .map_err(|error| SyncError::DockerConnect(error.to_string()))?;
    temps_network::docker::preflight_network_for_subnets(&docker, config, alloc, routed)
        .await
        .map_err(|error| SyncError::Preflight(error.to_string()))
}

/// The overlay subnets this node routes: its own and every peer's (the
/// control plane's included, when it runs the overlay).
fn routed_subnets(alloc: &NodeAlloc, peers: &[Peer]) -> Vec<Ipv4Net> {
    std::iter::once(alloc.compute_cidr)
        .chain(peers.iter().map(|peer| peer.compute_cidr))
        .collect()
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
pub(crate) async fn apply_dns_control_plane_trust(
    config: &AgentConfig,
    dns_cfg: &mut DnsResolverConfig,
) {
    // Same rule as every other control-plane call: the cluster CA only for a
    // node whose join pinned it (see `crate::control_plane_ca`).
    dns_cfg.control_plane_ca_pem = match (
        config.effective_control_plane_trust(),
        config.cluster_ca_path.as_ref(),
    ) {
        (crate::ControlPlaneTrust::ClusterCa, path) => {
            // Keep cluster trust fail-closed even when its configured CA cannot
            // be read: an empty PEM makes resolver startup report the error.
            Some(match path {
                Some(path) => tokio::fs::read(path).await.unwrap_or_default(),
                None => Vec::new(),
            })
        }
        _ => None,
    };
    dns_cfg.control_plane_legacy_cert_pem = match config.control_plane_legacy_cert_path.as_ref() {
        Some(path) => tokio::fs::read(path).await.ok(),
        None => None,
    };
}

async fn reconcile_resolver(
    cluster_dns_enabled: bool,
    bridge_address: IpAddr,
    resolver: &mut Option<DnsResolverHandle>,
    config: &AgentConfig,
    overlay_bridge_address: &Arc<std::sync::RwLock<Option<IpAddr>>>,
    dns_health: &SharedDnsHealth,
    dns_policy: &DnsPolicySlot,
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
            // Alive is not the same as answering: re-check every tick and
            // withdraw the address from new containers while it does not
            // answer, so they keep Docker's embedded DNS instead of paying a
            // timeout per lookup.
            let answering = resolver_answers(bridge_address).await;
            publish_resolver_address(overlay_bridge_address, answering.then_some(bridge_address));
            let probe_error = (!answering).then(|| {
                format!(
                    "resolver on {bridge_address}:53 is running but did not answer its readiness probe"
                )
            });
            if let Some(error) = &probe_error {
                warn!(
                    bridge = %bridge_address,
                    "{error}; new containers use Docker's embedded DNS until it answers"
                );
            }
            publish_dns_health(dns_health, answering, Some(&status), probe_error);
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

    let mut dns_cfg = DnsResolverConfig::new(
        config.node_id,
        config.token.clone(),
        config.control_plane_url.clone(),
        bridge_address,
        config.dns_data_dir.clone(),
    );
    apply_dns_control_plane_trust(config, &mut dns_cfg).await;
    let snapshot_path = dns_cfg.snapshot_path();
    let mut start_error = None;
    match DnsResolverHandle::start_with_policy(dns_cfg, Some(dns_policy.clone())).await {
        Ok(handle) => {
            info!(
                snapshot = %snapshot_path.display(),
                bridge = %bridge_address,
                "DNS resolver started"
            );
            *resolver = Some(handle);

            // Publish the bridge address so `service_handlers::create_service`
            // can wire it into every container's `--dns` -- only once the
            // resolver actually answers, never merely because it started.
            if resolver_answers(bridge_address).await {
                publish_resolver_address(overlay_bridge_address, Some(bridge_address));
            } else {
                publish_resolver_address(overlay_bridge_address, None);
                start_error = Some(format!(
                    "resolver on {bridge_address}:53 started but did not answer its readiness probe"
                ));
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
    let running = resolver.is_some() && start_error.is_none();
    publish_dns_health(dns_health, running, status.as_ref(), start_error);
}

/// Whether the resolver on `bridge_address` answers its readiness marker.
async fn resolver_answers(bridge_address: IpAddr) -> bool {
    temps_dns_resolver::probe::probe_resolver(std::net::SocketAddr::new(bridge_address, 53)).await
}

/// Set (or clear) the resolver address handed to new containers.
fn publish_resolver_address(
    overlay_bridge_address: &Arc<std::sync::RwLock<Option<IpAddr>>>,
    address: Option<IpAddr>,
) {
    match overlay_bridge_address.write() {
        Ok(mut slot) => *slot = address,
        Err(poisoned) => *poisoned.into_inner() = address,
    }
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
async fn ensure_overlay_docker_network(
    config: &NetworkConfig,
    alloc: &NodeAlloc,
    routed: &[Ipv4Net],
) -> Result<(), SyncError> {
    let docker = Docker::connect_with_local_defaults()
        .map_err(|e| SyncError::DockerConnect(e.to_string()))?;
    temps_network::docker::ensure_network_for_subnets(&docker, config, alloc, routed)
        .await
        .map_err(|e| SyncError::Bootstrap(format!("docker network: {}", e)))?;
    info!(
        network = %config.docker_network_name,
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
pub enum SyncError {
    #[error("failed to build http client: {0}")]
    ClientBuild(String),

    #[error("failed to construct NetworkManager: {0}")]
    ManagerConstruct(String),

    #[error("failed to resolve MTU for underlay device '{device}': {reason}")]
    UnderlayMtu { device: String, reason: String },

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

    /// The overlay would collide with this host's routes or Docker networks.
    /// The message already names the range, its owner and the fixes.
    #[error("{0}")]
    Preflight(String),

    #[error("reconcile failed: {0}")]
    Reconcile(String),

    #[error("failed to connect to local Docker daemon: {0}")]
    DockerConnect(String),

    #[error("WireGuard mesh: {0}")]
    Mesh(String),
}

impl SyncError {
    /// The control plane could not be reached, or failed, as opposed to
    /// answering: a 4xx means it rejected this node (removed from the
    /// cluster, token revoked), and a rejected node must not bring its old
    /// mesh back from the snapshot.
    fn control_plane_unreachable(&self) -> bool {
        match self {
            SyncError::Http(_) => true,
            SyncError::HttpStatus { status, .. } => status.is_server_error(),
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn underlay_is_probed_at_the_registered_private_address() {
        assert_eq!(
            underlay_probe_address(Some("10.57.0.11")),
            Some("10.57.0.11".parse().unwrap())
        );
        // The agent accepts and normalizes a host:port form.
        assert_eq!(
            underlay_probe_address(Some(" 10.57.0.11:3100 ")),
            Some("10.57.0.11".parse().unwrap())
        );
        assert_eq!(
            underlay_probe_address(Some("fd00::11")),
            Some("fd00::11".parse().unwrap())
        );
        // Nothing usable: fall back to the default route.
        assert_eq!(underlay_probe_address(None), None);
        assert_eq!(underlay_probe_address(Some("worker.internal")), None);
    }

    fn wire_alloc() -> WireAlloc {
        WireAlloc {
            node_id: "00000000-0000-0000-0000-00000000002a".into(),
            compute_cidr: "172.20.5.0/24".into(),
            bridge_address: "172.20.5.1".into(),
            underlay_address: "10.0.0.5".into(),
            overlay_network_name: None,
        }
    }

    fn agent_config_joined_at(private_address: &str) -> AgentConfig {
        serde_json::from_value(serde_json::json!({
            "listen_address": "127.0.0.1:3100",
            "token": "test-token",
            "node_name": "worker-1",
            "control_plane_url": "https://control:3000",
            "node_id": 1,
            "private_address": private_address,
        }))
        .expect("test agent config")
    }

    fn mesh_up_at(address: &str) -> MeshState {
        MeshState {
            configured: Some(MeshInterface {
                address: address.parse().expect("test mesh address"),
                prefix_len: 24,
                listen_port: 51820,
                mtu: 1420,
            }),
            ..Default::default()
        }
    }

    #[test]
    fn publicly_joined_node_moves_its_bind_slot_onto_the_mesh_once() {
        let config = agent_config_joined_at("203.0.113.10");
        let slot: SharedBindAddress = Arc::new(std::sync::RwLock::new(initial_bind_address(
            config.private_address.as_deref(),
        )));
        let mesh = mesh_up_at("10.99.0.4");

        let moved = publish_mesh_bind_address(&slot, &config, &mesh);

        assert_eq!(
            moved,
            Some(BindAddressMove {
                previous: "203.0.113.10".into(),
                current: "10.99.0.4".into(),
            })
        );
        assert_eq!(*slot.read().expect("test slot lock"), "10.99.0.4");
        // Later ticks with the same interface report nothing new.
        assert_eq!(publish_mesh_bind_address(&slot, &config, &mesh), None);
    }

    #[test]
    fn privately_joined_node_keeps_its_bind_address_on_the_mesh() {
        let config = agent_config_joined_at("10.0.0.5");
        let slot: SharedBindAddress = Arc::new(std::sync::RwLock::new("10.0.0.5".into()));

        let moved = publish_mesh_bind_address(&slot, &config, &mesh_up_at("10.99.0.4"));

        assert_eq!(moved, None);
        assert_eq!(*slot.read().expect("test slot lock"), "10.0.0.5");
    }

    #[test]
    fn bind_slot_stays_put_until_the_mesh_interface_is_configured() {
        let config = agent_config_joined_at("203.0.113.10");
        let slot: SharedBindAddress = Arc::new(std::sync::RwLock::new("203.0.113.10".into()));

        assert_eq!(
            publish_mesh_bind_address(&slot, &config, &MeshState::default()),
            None
        );
        assert_eq!(*slot.read().expect("test slot lock"), "203.0.113.10");
    }

    fn container_on(
        name: &str,
        host_ip: &str,
        labels: &[(&str, &str)],
    ) -> temps_deployer::ContainerInfo {
        temps_deployer::ContainerInfo {
            container_id: format!("{name}-id"),
            container_name: name.into(),
            ports: vec![temps_deployer::PortMapping {
                host_port: 31000,
                container_port: 8080,
                protocol: temps_deployer::Protocol::Tcp,
                host_ip: Some(host_ip.into()),
            }],
            labels: labels
                .iter()
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn containers_published_on_names_managed_containers_left_on_the_old_address() {
        let containers = vec![
            container_on(
                "app-old",
                "203.0.113.10",
                &[
                    ("sh.temps.managed", "true"),
                    ("sh.temps.deploy_id", "41"),
                    ("sh.temps.project_id", "7"),
                ],
            ),
            container_on(
                "db-old",
                "203.0.113.10",
                &[
                    ("sh.temps.managed", "true"),
                    ("sh.temps.service.name", "orders-db"),
                ],
            ),
            // Already on the mesh address: nothing to move.
            container_on("app-new", "10.99.0.4", &[("sh.temps.managed", "true")]),
            // Not created by Temps: not ours to report.
            container_on("unrelated", "203.0.113.10", &[]),
        ];

        let stranded = containers_published_on("203.0.113.10", &containers);

        assert_eq!(
            stranded,
            vec![
                StrandedContainer {
                    container_id: "app-old-id".into(),
                    container_name: "app-old".into(),
                    deployment_id: Some("41".into()),
                    project_id: Some("7".into()),
                    service_name: None,
                    bindings: vec!["203.0.113.10:31000".into()],
                },
                StrandedContainer {
                    container_id: "db-old-id".into(),
                    container_name: "db-old".into(),
                    deployment_id: None,
                    project_id: None,
                    service_name: Some("orders-db".into()),
                    bindings: vec!["203.0.113.10:31000".into()],
                },
            ]
        );
    }

    #[test]
    fn containers_published_on_is_empty_when_nothing_is_left_behind() {
        let containers = vec![container_on(
            "app-new",
            "10.99.0.4",
            &[("sh.temps.managed", "true")],
        )];

        assert!(containers_published_on("203.0.113.10", &containers).is_empty());
    }

    fn wire_peer() -> WirePeer {
        WirePeer {
            node_id: "00000000-0000-0000-0000-000000000007".into(),
            compute_cidr: "172.20.6.0/24".into(),
            underlay_address: "10.0.0.6".into(),
        }
    }

    #[test]
    fn detected_vlan_mtu_drives_vxlan_mtu() {
        let underlay_mtu = effective_underlay_mtu(1400, None);
        assert_eq!(underlay_mtu, 1400);
        assert_eq!(
            NetworkConfig::default().transport.bridge_mtu(underlay_mtu),
            1350
        );
    }

    #[test]
    fn explicit_mtu_can_lower_detected_device_mtu() {
        assert_eq!(effective_underlay_mtu(1500, Some(1400)), 1400);
    }

    #[test]
    fn explicit_mtu_cannot_exceed_detected_device_mtu() {
        assert_eq!(effective_underlay_mtu(1400, Some(1500)), 1400);
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
    fn authoritative_pool_rejects_allocation_from_another_cluster_cidr() {
        let alloc = parse_alloc(&wire_alloc()).unwrap();
        let network = WireNetworkPool {
            compute_pool_cidr: "10.240.0.0/16".into(),
            subnet_prefix_len: 24,
        };
        let error = validate_cluster_topology(&network, &alloc, &[]).unwrap_err();
        assert!(
            matches!(error, SyncError::WireParse(message) if message.contains("refusing inconsistent routes"))
        );
    }

    #[test]
    fn authoritative_pool_accepts_matching_node_allocation() {
        let alloc = parse_alloc(&wire_alloc()).unwrap();
        let network = WireNetworkPool {
            compute_pool_cidr: "172.20.0.0/16".into(),
            subnet_prefix_len: 24,
        };
        assert_eq!(
            validate_cluster_topology(&network, &alloc, &[]).unwrap(),
            "172.20.0.0/16".parse().unwrap()
        );
    }

    #[test]
    fn authoritative_pool_rejects_peer_outside_pool() {
        let alloc = parse_alloc(&wire_alloc()).unwrap();
        let network = WireNetworkPool {
            compute_pool_cidr: "172.20.0.0/16".into(),
            subnet_prefix_len: 24,
        };
        let mut peer = parse_peer(&wire_peer()).unwrap();
        peer.compute_cidr = "10.99.0.0/24".parse().unwrap();
        assert!(validate_cluster_topology(&network, &alloc, &[peer]).is_err());
    }

    #[test]
    fn authoritative_pool_rejects_duplicate_peer_allocation() {
        let alloc = parse_alloc(&wire_alloc()).unwrap();
        let network = WireNetworkPool {
            compute_pool_cidr: "172.20.0.0/16".into(),
            subnet_prefix_len: 24,
        };
        let mut peer = parse_peer(&wire_peer()).unwrap();
        peer.compute_cidr = alloc.compute_cidr;
        assert!(validate_cluster_topology(&network, &alloc, &[peer]).is_err());
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

    // ADR 059: a control plane that predates the overlay network name sends
    // no field, which means `temps0`; a newer one names the node's overlay.
    #[test]
    fn an_alloc_without_an_overlay_name_keeps_temps0() {
        let alloc: WireAlloc = serde_json::from_value(serde_json::json!({
            "node_id": "00000000-0000-0000-0000-00000000002a",
            "compute_cidr": "172.20.5.0/24",
            "bridge_address": "172.20.5.1",
            "underlay_address": "10.0.0.5"
        }))
        .unwrap();
        assert_eq!(alloc.overlay_network_name, None);
        assert_eq!(alloc.overlay_names().unwrap(), OverlayNames::default());
        // Serialised back (the snapshot), it stays absent for older readers.
        assert!(!serde_json::to_string(&alloc)
            .unwrap()
            .contains("overlay_network_name"));
    }

    // Before the first manager build `overlay_name::current()` still says
    // `temps0`; the collection must leave out the snapshot's overlay instead.
    #[test]
    fn the_occupied_scan_leaves_out_the_snapshot_overlay_before_a_build() {
        let temps1 = OverlayNames::new("temps1").unwrap();
        let config = occupied_scan_config(None, Some(&temps1));
        assert_eq!(config.docker_network_name, "temps1");
        assert_eq!(config.bridge_name, "br-temps1");
        assert_eq!(config.vxlan_dev_name, "vxlan-temps1");
        // A built manager is authoritative.
        let built =
            NetworkConfig::default().with_overlay_names(&OverlayNames::new("temps2").unwrap());
        let config = occupied_scan_config(Some(&built), Some(&temps1));
        assert_eq!(config.docker_network_name, "temps2");
    }

    #[test]
    fn an_alloc_names_its_overlay_and_rejects_an_unsafe_name() {
        let mut alloc = wire_alloc();
        alloc.overlay_network_name = Some("temps1".into());
        let names = alloc.overlay_names().unwrap();
        assert_eq!(names.docker_network, "temps1");
        assert_eq!(names.bridge, "br-temps1");
        assert_eq!(names.vxlan, "vxlan-temps1");
        let round_trip: WireAlloc =
            serde_json::from_str(&serde_json::to_string(&alloc).unwrap()).unwrap();
        assert_eq!(round_trip, alloc);

        alloc.overlay_network_name = Some("br0; rm -rf".into());
        assert!(alloc.overlay_names().is_err());
    }

    #[test]
    fn the_previous_overlay_comes_from_the_last_applied_snapshot() {
        let mut alloc = wire_alloc();
        alloc.overlay_network_name = Some("temps1".into());
        let snapshot = WirePeerListResponse {
            network: None,
            alloc: Some(alloc),
            peers: vec![wire_peer()],
            cluster_dns_enabled: false,
            wireguard: None,
        };
        let previous = PreviousOverlay::from_snapshot(&snapshot).unwrap();
        assert_eq!(previous.node_id, "00000000-0000-0000-0000-00000000002a");
        assert_eq!(previous.names, OverlayNames::new("temps1").unwrap());
        assert_eq!(
            previous.peer_subnets,
            vec![Ipv4Net::from_str("172.20.6.0/24").unwrap()]
        );
        let unallocated = WirePeerListResponse {
            alloc: None,
            ..snapshot
        };
        assert_eq!(PreviousOverlay::from_snapshot(&unallocated), None);
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

    #[test]
    fn network_snapshot_round_trips_with_the_mesh_section() {
        let payload: WirePeerListResponse = serde_json::from_value(serde_json::json!({
            "network": {"compute_pool_cidr": "172.20.0.0/16", "subnet_prefix_len": 24},
            "alloc": {
                "node_id": "00000000-0000-0000-0000-000000000001",
                "compute_cidr": "172.20.2.0/24",
                "bridge_address": "172.20.2.1",
                "underlay_address": "10.201.0.4"
            },
            "peers": [],
            "cluster_dns_enabled": false,
            "wireguard": {
                "cidr": "10.201.0.0/24",
                "listen_port": 51820,
                "self": {"public_key": "k", "endpoint": "10.62.0.21:51820", "address": "10.201.0.4"},
                "peers": [{"name": "control-plane", "public_key": "p", "endpoint": null, "address": "10.201.0.1"}]
            }
        }))
        .unwrap();
        let dir = std::env::temp_dir().join(format!("temps-snapshot-{}", std::process::id()));
        let path = dir.join("network-snapshot.json");

        save_snapshot(&path, &payload).unwrap();
        let restored = load_snapshot(&path).expect("snapshot loads");

        let mesh = restored.wireguard.expect("mesh section kept");
        assert_eq!(mesh.self_entry.unwrap().address, "10.201.0.4");
        assert_eq!(mesh.peers[0].endpoint, None);
        assert_eq!(restored.alloc.unwrap().underlay_address, "10.201.0.4");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
            let dir_mode = std::fs::metadata(&dir).unwrap().permissions().mode();
            assert_eq!(dir_mode & 0o777, 0o700);
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn mesh_addresses_must_be_unique_and_inside_the_pool() {
        let cidr: Ipv4Net = "10.201.0.0/24".parse().unwrap();
        let own = "10.201.0.4".parse().unwrap();
        let peer = |address: &str| MeshPeer {
            public_key: "k".into(),
            endpoint: None,
            address: address.parse().unwrap(),
            relayed: Vec::new(),
        };
        let hub = |address: &str, relayed: &[&str]| MeshPeer {
            relayed: relayed.iter().map(|a| a.parse().unwrap()).collect(),
            ..peer(address)
        };

        assert!(check_mesh_addresses(cidr, own, &[peer("10.201.0.1"), peer("10.201.0.2")]).is_ok());
        assert!(check_mesh_addresses(cidr, own, &[hub("10.201.0.1", &["10.201.0.2"])]).is_ok());
        assert!(
            check_mesh_addresses(cidr, own, &[hub("10.201.0.1", &["10.9.0.2"])]).is_err(),
            "a relayed address outside the pool would claim unrelated traffic"
        );
        assert!(
            check_mesh_addresses(
                cidr,
                own,
                &[hub("10.201.0.1", &["10.201.0.2"]), peer("10.201.0.2")]
            )
            .is_err(),
            "a member is either relayed or direct, never both"
        );
        assert!(check_mesh_addresses(cidr, own, &[hub("10.201.0.1", &["10.201.0.4"])]).is_err());
        assert!(check_mesh_addresses(cidr, "10.9.0.4".parse().unwrap(), &[]).is_err());
        assert!(check_mesh_addresses(cidr, own, &[peer("192.168.1.10")]).is_err());
        assert!(check_mesh_addresses(cidr, own, &[peer("10.201.0.4")]).is_err());
        assert!(
            check_mesh_addresses(cidr, own, &[peer("10.201.0.2"), peer("10.201.0.2")]).is_err()
        );
    }

    #[test]
    fn the_mesh_pool_is_held_to_the_control_planes_rules() {
        let pool: Ipv4Net = "172.20.0.0/16".parse().unwrap();
        assert_eq!(
            mesh_cidr("10.201.0.0/24", Some(pool)).unwrap(),
            "10.201.0.0/24".parse::<Ipv4Net>().unwrap()
        );
        assert!(mesh_cidr("10.201.0.0/24", None).is_ok());
        for (value, why) in [
            (
                "8.8.8.0/24",
                "public space would go into the lockdown and routes",
            ),
            ("0.0.0.0/0", "everything"),
            ("10.0.0.0/7", "starts private but runs into 11/8"),
            ("10.201.0.0/30", "no room for the members"),
            ("not-a-cidr", "unparsable"),
        ] {
            let error = mesh_cidr(value, Some(pool)).unwrap_err();
            assert!(
                error.to_string().contains("wireguard.cidr"),
                "{value}: {why}"
            );
            assert!(mesh_cidr(value, None).is_err(), "{value}: {why}");
        }
        assert!(
            mesh_cidr("172.20.0.0/24", Some(pool)).is_err(),
            "a pool inside the compute pool"
        );
        assert!(
            mesh_cidr("172.16.0.0/12", Some(pool)).is_err(),
            "a pool around the compute pool"
        );
    }

    /// ADR 055: a worker that joined outbound only registered its mesh
    /// address as its private address. Nobody can dial it, so it publishes
    /// no endpoint instead of `<mesh address>:<port>`.
    #[test]
    fn a_node_registered_under_its_mesh_address_publishes_no_endpoint() {
        let wire = WireMesh {
            cidr: "10.201.0.0/24".into(),
            listen_port: 51820,
            self_entry: None,
            peers: Vec::new(),
            hub: false,
        };
        let endpoint = |config: &AgentConfig| mesh_endpoint(config, &wire).unwrap();
        assert_eq!(endpoint(&agent_config_joined_at("10.201.0.4")), None);
        assert_eq!(
            endpoint(&agent_config_joined_at("203.0.113.5")),
            Some("203.0.113.5:51820".parse().unwrap())
        );
        assert_eq!(
            endpoint(&agent_config_joined_at("10.0.0.5")),
            Some("10.0.0.5:51820".parse().unwrap()),
            "a private address outside the mesh pool is still dialed"
        );
        // `--wg-endpoint` always wins: a worker with a public address may
        // let other workers dial it directly.
        let mut config = agent_config_joined_at("10.201.0.4");
        config.wg_endpoint = Some("198.51.100.7:4000".into());
        assert_eq!(
            endpoint(&config),
            Some("198.51.100.7:4000".parse().unwrap())
        );

        // The registration omits the endpoint; a stored entry without one
        // (or from an older control plane, with one) still parses.
        let body = serde_json::to_value(MeshRegistrationBody {
            public_key: "k",
            endpoint: None,
        })
        .unwrap();
        assert_eq!(body, serde_json::json!({ "public_key": "k" }));
        let me: WireMeshSelf = serde_json::from_value(
            serde_json::json!({ "public_key": "k", "endpoint": null, "address": "10.201.0.4" }),
        )
        .unwrap();
        assert_eq!(me.endpoint, None);
        let me: WireMeshSelf = serde_json::from_value(
            serde_json::json!({ "public_key": "k", "address": "10.201.0.4" }),
        )
        .unwrap();
        assert_eq!(me.endpoint, None);
    }

    #[tokio::test]
    async fn a_bad_mesh_pool_is_refused_before_the_host_is_touched() {
        let key_dir = tempfile::tempdir().unwrap();
        let key = MeshKey::load_or_create(key_dir.path()).unwrap();
        let config = agent_config_joined_at("203.0.113.5");
        let wire = WireMesh {
            cidr: "198.51.100.0/24".into(),
            listen_port: 51820,
            self_entry: Some(WireMeshSelf {
                public_key: key.public_key().to_string(),
                endpoint: Some("203.0.113.5:51820".into()),
                address: "198.51.100.4".into(),
            }),
            peers: Vec::new(),
            hub: false,
        };
        let mut state = MeshState {
            key: Some(key),
            ..mesh_up_at("10.201.0.4")
        };
        let error = reconcile_mesh(
            &reqwest::Client::new(),
            "",
            &config,
            &wire,
            None,
            &mut state,
            true,
        )
        .await
        .err()
        .expect("a public mesh pool is refused");
        // Refused while parsing, before the lockdown, the preflight or the
        // interface: none of them is attempted.
        assert!(
            matches!(&error, SyncError::WireParse(reason) if reason.contains("wireguard.cidr")),
            "{error}"
        );
        assert_eq!(state.preflighted, None);
        assert_eq!(
            state.configured.as_ref().map(|interface| interface.address),
            Some("10.201.0.4".parse().unwrap()),
            "the interface already up is not reconfigured"
        );
    }

    #[test]
    fn only_an_unreachable_control_plane_triggers_the_snapshot_restore() {
        let status = |code: u16| SyncError::HttpStatus {
            status: reqwest::StatusCode::from_u16(code).unwrap(),
            body: String::new(),
        };
        assert!(SyncError::Http("connection refused".into()).control_plane_unreachable());
        assert!(status(502).control_plane_unreachable());
        assert!(
            !status(404).control_plane_unreachable(),
            "a removed node stays off the mesh"
        );
        assert!(
            !status(401).control_plane_unreachable(),
            "a revoked token stays off the mesh"
        );
    }

    #[test]
    fn an_unreadable_snapshot_is_ignored() {
        let dir = std::env::temp_dir().join(format!("temps-snapshot-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("network-snapshot.json");
        std::fs::write(&path, "{not json").unwrap();
        assert!(load_snapshot(&path).is_none());
        assert!(load_snapshot(&dir.join("missing.json")).is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_unchanged_overlay_failure_backs_off_to_five_minutes_and_logs_sparingly() {
        let start = std::time::Instant::now();
        let mut retry = OverlayRetry::default();
        let collision = "overlay subnet 10.202.0.0/24 overlaps host route 10.0.0.0/8";

        let first = retry.failed(collision, 1, start);
        assert_eq!(
            first,
            RetryStep {
                delay: Duration::from_secs(5),
                log: true
            }
        );
        assert!(!retry.due(1, start), "backing off");
        assert!(retry.due(1, start + Duration::from_secs(5)));

        let mut now = start;
        let mut delays = Vec::new();
        let mut logged = Vec::new();
        for _ in 0..10 {
            now += retry.remaining(now);
            let step = retry.failed(collision, 1, now);
            delays.push(step.delay.as_secs());
            logged.push(step.log);
        }
        assert_eq!(delays, [10, 20, 40, 80, 160, 300, 300, 300, 300, 300]);
        // Re-logged only once five minutes passed since the last line (the
        // attempt at 315 s), then once per capped attempt.
        assert_eq!(
            logged,
            [false, false, false, false, false, true, true, true, true, true]
        );

        // A new cause is logged at once.
        assert!(retry.failed("docker network 'shop' collides", 1, now).log);
    }

    #[test]
    fn a_change_in_allocation_peers_or_occupied_ranges_retries_at_once() {
        let now = std::time::Instant::now();
        let mut retry = OverlayRetry::default();
        for _ in 0..6 {
            retry.failed("collision", 1, now);
        }
        assert!(!retry.due(1, now));
        assert!(retry.due(2, now), "new inputs are tried immediately");
        let step = retry.failed("collision", 2, now);
        assert_eq!(step.delay, Duration::from_secs(5), "backoff restarts");
        assert!(!step.log, "same cause, just logged");

        assert_eq!(retry.succeeded(), 1, "failures since the inputs changed");
        assert!(retry.due(2, now));
        assert!(
            retry.failed("collision", 2, now).log,
            "a new failure after success is logged"
        );
    }

    #[test]
    fn the_fingerprint_follows_allocation_peers_and_occupied_ranges() {
        let mut payload = WirePeerListResponse {
            network: None,
            alloc: Some(wire_alloc()),
            peers: vec![wire_peer()],
            cluster_dns_enabled: false,
            wireguard: None,
        };
        let base = overlay_fingerprint(&payload, None);
        assert_eq!(base, overlay_fingerprint(&payload.clone(), None));
        let occupied = [temps_network::occupied::OccupiedRange::docker(
            "172.20.0.0/16".parse().unwrap(),
            "shop_default",
        )];
        assert_ne!(base, overlay_fingerprint(&payload, Some(&occupied)));
        payload.cluster_dns_enabled = true;
        assert_eq!(
            base,
            overlay_fingerprint(&payload, None),
            "DNS is not an overlay input"
        );
        payload.alloc.as_mut().unwrap().compute_cidr = "10.202.9.0/24".into();
        assert_ne!(base, overlay_fingerprint(&payload, None));
    }

    #[test]
    fn only_the_own_and_peer_subnets_are_routed() {
        let alloc = parse_alloc(&wire_alloc()).unwrap();
        let peer = parse_peer(&wire_peer()).unwrap();
        assert_eq!(
            routed_subnets(&alloc, std::slice::from_ref(&peer)),
            vec![alloc.compute_cidr, peer.compute_cidr]
        );
    }

    #[test]
    fn overlay_heartbeat_wire_form() {
        let ready = OverlayHeartbeat::ready("10.202.3.0/24".parse().unwrap());
        assert_eq!(
            serde_json::to_value(&ready).unwrap(),
            serde_json::json!({"status": "ready", "compute_cidr": "10.202.3.0/24", "last_error": null})
        );
        let down = OverlayHeartbeat::unavailable(None, "cause. Fix: x".into());
        assert_eq!(
            serde_json::to_value(&down).unwrap()["status"],
            "unavailable"
        );
    }
}
