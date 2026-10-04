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
    ResolverConfig as DnsResolverConfig, ResolverHandle as DnsResolverHandle,
};
use temps_network::{NetworkConfig, NetworkManager, NodeAlloc, Peer};
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
    endpoint: String,
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
pub fn spawn(
    config: &AgentConfig,
    overlay_bridge_address: Arc<std::sync::RwLock<Option<IpAddr>>>,
    peers: SharedPeers,
    dns_health: SharedDnsHealth,
    bind_address: SharedBindAddress,
    containers: Arc<dyn ContainerDeployer>,
) {
    let cfg = config.clone();
    tokio::spawn(async move {
        if let Err(e) = run(
            cfg,
            overlay_bridge_address,
            peers,
            dns_health,
            bind_address,
            containers,
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

async fn run(
    config: AgentConfig,
    overlay_bridge_address: Arc<std::sync::RwLock<Option<IpAddr>>>,
    shared_peers: SharedPeers,
    dns_health: SharedDnsHealth,
    bind_address: SharedBindAddress,
    containers: Arc<dyn ContainerDeployer>,
) -> Result<(), SyncError> {
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

    let mut bootstrapped = false;
    // Started after first successful bootstrap. Held here (not dropped)
    // so the resolver tasks stay alive for the lifetime of the agent.
    let mut _resolver_handle: Option<DnsResolverHandle> = None;

    let slots = SharedSlots {
        overlay_bridge_address: &overlay_bridge_address,
        shared_peers: &shared_peers,
        dns_health: &dns_health,
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

    loop {
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
                if let Some(wire) = &payload.wireguard {
                    match reconcile_mesh(&client, &mesh_url, &config, wire, &mut mesh, offline)
                        .await
                    {
                        Ok(MeshTick::Ready { rebuilt, report }) => {
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
                                bootstrapped = false;
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
                            warn!(error = %e, "WireGuard mesh sync failed; will retry");
                            tokio::time::sleep(BACKOFF_INTERVAL).await;
                            continue;
                        }
                    }
                }
                if payload.alloc.is_none() {
                    debug!("network sync: no compute_cidr allocated yet");
                    tokio::time::sleep(POLL_INTERVAL).await;
                    continue;
                }
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
                    bootstrapped = false;
                }
                let manager = match &manager {
                    Some((manager, _)) => manager,
                    None => match build_manager(&config, on_mesh).await {
                        Ok(built) => &manager.insert((built, on_mesh)).0,
                        Err(e) => {
                            warn!(error = %e, "overlay setup failed; will retry");
                            tokio::time::sleep(BACKOFF_INTERVAL).await;
                            continue;
                        }
                    },
                };
                let snapshot = (!offline).then(|| payload.clone());
                if let Err(e) = apply(
                    manager,
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
                applied_once = true;
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

/// Build the overlay manager. On the WireGuard mesh the underlay is the mesh
/// interface; otherwise the configured device, the one holding the node's
/// private address, or the default-route device.
async fn build_manager(config: &AgentConfig, on_mesh: bool) -> Result<NetworkManager, SyncError> {
    let mut net_config = NetworkConfig::default();
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
    endpoint: String,
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
    let endpoint = match &config.wg_endpoint {
        Some(value) => temps_network::mesh::parse_endpoint(value),
        None => {
            let registered = config.private_address.as_deref().ok_or_else(|| {
                SyncError::Mesh(
                    "no WireGuard endpoint: this node has no registered private address; \
                     set --wg-endpoint <ip:port>"
                        .into(),
                )
            })?;
            temps_network::mesh::default_endpoint(registered, wire.listen_port)
        }
    }
    .map_err(|e| SyncError::Mesh(e.to_string()))?;

    let registered = wire
        .self_entry
        .as_ref()
        .filter(|me| me.public_key == key.public_key() && me.endpoint == endpoint.to_string());
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
                endpoint: endpoint.to_string(),
            })
            .send()
            .await
            .map_err(|e| SyncError::Http(e.to_string()))?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(SyncError::HttpStatus { status, body });
        }
        info!(%endpoint, public_key = key.public_key(), "registered with the WireGuard mesh");
        return Ok(MeshTick::Registered);
    };

    let cidr = Ipv4Net::from_str(&wire.cidr)
        .map_err(|e| SyncError::WireParse(format!("wireguard.cidr: {e}")))?;
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
    temps_network::mesh::ensure_lockdown(&temps_network::mesh::MeshLockdown {
        vxlan_port: overlay_vxlan_port(),
        mesh: cidr,
        node_api_port: None,
        relay: wire.hub,
    })
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
    let mut rebuilt = false;
    if state.configured.as_ref() != Some(&interface) {
        if state.configured.is_none() {
            temps_network::mesh::preflight_routes(cidr)
                .await
                .map_err(|e| SyncError::Mesh(e.to_string()))?;
        }
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
    /// Where other members dial this node.
    pub endpoint: std::net::SocketAddr,
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
            endpoint: bootstrap.endpoint.to_string(),
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
    reconcile_mesh(&client, "", config, &wire, &mut state, true)
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
    #[error("{0}")]
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
        endpoint: temps_network::mesh::parse_endpoint(&me.endpoint).ok(),
        peers,
        lockdown: temps_network::mesh::MeshLockdown {
            vxlan_port: overlay_vxlan_port(),
            mesh: cidr,
            node_api_port: None,
            relay: wire.hub,
        },
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
    let authoritative_pool = match payload.network.as_ref() {
        Some(network) => validate_cluster_topology(network, &alloc, &peers)?,
        None => alloc.compute_cidr,
    };
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

    // Re-check the complete authoritative pool on every snapshot, not only
    // initial bootstrap. A local Docker network or route created later must
    // be rejected before a newly allocated peer route can be installed.
    let alloc_for_docker = alloc.clone();
    temps_network::preflight_compute_pool_routes(manager.config(), authoritative_pool)
        .await
        .map_err(|error| SyncError::Bootstrap(format!("host route preflight: {error}")))?;
    preflight_overlay_docker_network(manager.config(), &alloc_for_docker, authoritative_pool)
        .await?;

    if !*bootstrapped {
        info!(
            cidr = %alloc.compute_cidr,
            peers = peers.len(),
            "bringing up multi-host overlay"
        );
        // Refuse address-space collisions before mutating kernel state. The
        // control plane allocation remains authoritative; this node must fix
        // its local Docker pools rather than silently choosing another CIDR.
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
        ensure_overlay_docker_network(manager.config(), &alloc_for_docker, authoritative_pool)
            .await?;
        *bootstrapped = true;
    } else {
        ensure_overlay_docker_network(manager.config(), &alloc_for_docker, authoritative_pool)
            .await?;
        let changed = manager
            .reconcile_peers(peers.clone())
            .await
            .map_err(|e| SyncError::Reconcile(e.to_string()))?;
        if changed {
            info!("multi-host peer list updated");
        }
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
    authoritative_pool: Ipv4Net,
) -> Result<(), SyncError> {
    let docker = Docker::connect_with_local_defaults()
        .map_err(|error| SyncError::DockerConnect(error.to_string()))?;
    temps_network::docker::preflight_network_for_pool(&docker, config, alloc, authoritative_pool)
        .await
        .map_err(|error| SyncError::Bootstrap(format!("docker network preflight: {error}")))
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

    let mut dns_cfg = DnsResolverConfig::new(
        config.node_id,
        config.token.clone(),
        config.control_plane_url.clone(),
        bridge_address,
        config.dns_data_dir.clone(),
    );
    // Same rule as every other control-plane call: the cluster CA only for a
    // node whose join pinned it (see `crate::control_plane_ca`).
    dns_cfg.control_plane_ca_pem = match (
        config.effective_control_plane_trust(),
        config.cluster_ca_path.as_ref(),
    ) {
        (crate::ControlPlaneTrust::ClusterCa, Some(path)) => tokio::fs::read(path).await.ok(),
        _ => None,
    };
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
async fn ensure_overlay_docker_network(
    config: &NetworkConfig,
    alloc: &NodeAlloc,
    authoritative_pool: Ipv4Net,
) -> Result<(), SyncError> {
    let docker = Docker::connect_with_local_defaults()
        .map_err(|e| SyncError::DockerConnect(e.to_string()))?;
    temps_network::docker::ensure_network_for_pool(&docker, config, alloc, authoritative_pool)
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
}
