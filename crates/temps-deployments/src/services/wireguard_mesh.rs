// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The managed WireGuard mesh as the Worker Nodes page sees it (ADR 048).
//!
//! [`WireguardMeshService::status`] answers "can nodes that only share the
//! internet with this control plane join it, and are the ones that did
//! connected?" — including, when the mesh is off, what is missing and how to
//! turn it on, so the page can onboard instead of hiding the option.
//! [`WireguardMeshService::enable`] turns the mesh on; the running
//! `temps serve` notices the setting change and brings its end up without a
//! restart.

use std::sync::Arc;
use std::time::SystemTime;

use sea_orm::DatabaseConnection;
use serde::{Deserialize, Serialize};
use temps_config::ConfigService;
use temps_core::node_address::is_private_node_address;
use temps_entities::nodes;
use temps_network::mesh::{MeshError, MeshPeerStatus, MeshSettings, LIVE_HANDSHAKE};
use temps_network::mesh_links::{self as ml, Hub, LinkState, Member};
use utoipa::ToSchema;

use crate::services::node_service::{NodeError, NodeService};

/// CLI equivalent of the enable action, run on the control-plane host.
pub const ENABLE_MESH_COMMAND: &str = "temps network setup-multi-node --wireguard";

/// Whether the cluster's mesh is carrying traffic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum WireguardMeshState {
    /// Off: nodes must reach the control plane and each other on a private
    /// network.
    Disabled,
    /// On, but the control plane has not brought its end up yet, so no node
    /// has moved onto the mesh.
    Starting,
    /// On, and the control plane's end is up and published to the nodes.
    Ready,
}

/// One node's standing on the mesh, as the control plane sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum WireguardMeshNodeConnection {
    /// The mesh is off.
    MeshOff,
    /// The node's agent has not registered a mesh key yet (not running, an
    /// older version, or still starting).
    NotRegistered,
    /// Registered, but the control plane has not brought its end up yet.
    WaitingForControlPlane,
    /// Handshook with the control plane within the last three minutes.
    Connected,
    /// Handshook once, but not within the last three minutes.
    Stale,
    /// Registered, but never handshook with the control plane: usually the
    /// mesh UDP port is blocked between them.
    NeverConnected,
    /// Handshake data could not be read on the control plane (see
    /// `handshake_error`).
    Unknown,
}

/// Outcome of one mesh check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum WireguardMeshCheckStatus {
    Pass,
    Warn,
    Fail,
    Info,
}

/// One thing the control plane can tell about a node's mesh link (ADR 048
/// D9), with the action that fixes it when it fails.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct WireguardMeshCheck {
    pub label: String,
    pub status: WireguardMeshCheckStatus,
    /// Rendered verbatim.
    pub detail: String,
    /// What fixes it; rendered verbatim.
    pub fix: Option<String>,
}

/// The control plane's end of the mesh.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct WireguardMeshControlPlaneEntry {
    /// Its mesh address.
    pub address: String,
    /// `ip:port` nodes dial. `None` when the control plane has no address
    /// nodes can reach (e.g. it runs on a laptop): it dials the nodes that
    /// publish an endpoint instead.
    pub endpoint: Option<String>,
    /// Whether `endpoint` is a private address, which nodes joining over the
    /// internet cannot reach.
    pub endpoint_is_private: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct WireguardMeshNodeStatus {
    pub node_id: i32,
    pub name: String,
    pub node_status: String,
    /// The address the node joined with (`temps join --private-address`).
    pub registered_address: String,
    /// Whether that address is private: the node shares a private network
    /// with the control plane rather than only the internet.
    pub registered_on_private_network: bool,
    pub mesh_address: Option<String>,
    /// `ip:port` other nodes dial for this node's WireGuard socket.
    pub endpoint: Option<String>,
    /// Where the control plane reaches this node's agent and published
    /// ports: the mesh address for a node joined over the internet.
    pub data_address: String,
    pub connection: WireguardMeshNodeConnection,
    /// RFC 3339.
    pub last_handshake_at: Option<String>,
    pub rx_bytes: Option<u64>,
    pub tx_bytes: Option<u64>,
    /// What the control plane can check about this node's link (empty while
    /// the mesh is off). The node's own view: `temps doctor mesh` on it.
    pub checks: Vec<WireguardMeshCheck>,
}

/// The member relaying for pairs that cannot reach each other (ADR 048 D4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WireguardMeshHubTarget {
    /// No hub: pairs that cannot reach each other stay disconnected.
    None,
    ControlPlane,
    Node {
        node_id: i32,
    },
}

impl WireguardMeshHubTarget {
    /// The hub this target names, `None` for no hub.
    pub fn hub(&self) -> Option<Hub> {
        match self {
            WireguardMeshHubTarget::None => None,
            WireguardMeshHubTarget::ControlPlane => Some(Hub::ControlPlane),
            WireguardMeshHubTarget::Node { node_id } => Some(Hub::Node(*node_id)),
        }
    }
}

/// The current hub.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct WireguardMeshHub {
    pub target: WireguardMeshHubTarget,
    /// `control-plane` or the node's name.
    pub name: String,
}

/// How one pair of mesh members is connected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum WireguardMeshLinkState {
    /// They handshook directly within the last three minutes.
    Direct,
    /// Their traffic goes through the hub: the direct link never came up.
    ViaHub,
    /// Direct, not handshaken yet; the hub takes over if it stays that way.
    Connecting,
    /// Direct, never handshaken, and nothing will change that: no hub is
    /// set, or one of them is not reporting (down, or an older agent).
    Unreachable,
}

/// One pair of mesh members.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct WireguardMeshLink {
    /// `control-plane` or a node name.
    pub a: String,
    pub b: String,
    /// `None` for the control plane.
    pub a_node_id: Option<i32>,
    pub b_node_id: Option<i32>,
    pub state: WireguardMeshLinkState,
    /// Most recent direct handshake either side reported (RFC 3339).
    pub last_handshake_at: Option<String>,
    /// What connects them when `state` is not `direct`, or what to do.
    /// Rendered verbatim.
    pub detail: Option<String>,
}

/// Response of `GET /nodes/wireguard`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct WireguardMeshStatusResponse {
    pub state: WireguardMeshState,
    /// Why the mesh is not ready, when it is not. Rendered verbatim.
    pub reason: Option<String>,
    /// Mesh address pool, when enabled.
    pub cidr: Option<String>,
    /// UDP port every node must accept from the others — the configured
    /// one even while the mesh is off, so it can be opened in advance.
    pub listen_port: u16,
    pub control_plane: Option<WireguardMeshControlPlaneEntry>,
    /// Whether `POST /nodes/wireguard` can turn the mesh on here.
    pub can_enable: bool,
    /// What prevents enabling it, when `can_enable` is false and the mesh is
    /// off. Rendered verbatim.
    pub enable_blocker: Option<String>,
    /// CLI equivalent of enabling, for the control-plane host.
    pub enable_command: String,
    /// The configured external URL: what `temps join` should point at.
    /// `null` when none is configured.
    pub join_url: Option<String>,
    /// Why handshake data is missing, when it is.
    pub handshake_error: Option<String>,
    pub nodes: Vec<WireguardMeshNodeStatus>,
    /// The mesh hub, if one is set.
    pub hub: Option<WireguardMeshHub>,
    /// Every pair of members that are both on the mesh.
    pub links: Vec<WireguardMeshLink>,
}

/// What an operator asked for when turning the mesh on. Omitted fields keep
/// their current value.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EnableMesh {
    pub cidr: Option<String>,
    pub listen_port: Option<u16>,
    pub node_api_port: Option<u16>,
}

#[derive(Debug, thiserror::Error)]
pub enum WireguardMeshError {
    /// Reading or changing the stored mesh state failed, or the change was
    /// refused (see `source`).
    #[error("could not {action}: {source}")]
    Mesh {
        action: &'static str,
        #[source]
        source: MeshError,
    },
    #[error("could not list the cluster's nodes: {0}")]
    Nodes(#[from] NodeError),
    /// This server cannot bring up the control plane's end of the mesh.
    #[error("{reason}")]
    UnavailableHere { reason: String },
}

fn mesh(action: &'static str) -> impl FnOnce(MeshError) -> WireguardMeshError {
    move |source| WireguardMeshError::Mesh { action, source }
}

/// Reads and changes the cluster's WireGuard mesh for the admin API.
pub struct WireguardMeshService {
    db: Arc<DatabaseConnection>,
    node_service: Arc<NodeService>,
    config_service: Arc<ConfigService>,
}

impl WireguardMeshService {
    pub fn new(
        db: Arc<DatabaseConnection>,
        node_service: Arc<NodeService>,
        config_service: Arc<ConfigService>,
    ) -> Self {
        Self {
            db,
            node_service,
            config_service,
        }
    }

    /// Mesh state, per-node connection and join onboarding.
    pub async fn status(&self) -> Result<WireguardMeshStatusResponse, WireguardMeshError> {
        let db = self.db.as_ref();
        let settings = temps_network::mesh::load_settings(db)
            .await
            .map_err(mesh("read the mesh settings"))?;
        let published = temps_network::mesh::published_control_plane(db)
            .await
            .map_err(mesh("read the control plane's mesh key"))?;
        let listen_port = match &settings {
            Some(settings) => settings.port,
            None => temps_network::mesh::configured_port(db)
                .await
                .map_err(mesh("read the mesh port"))?,
        };
        let (state, mut reason) = state_of(settings.as_ref(), published.is_some());
        if state == WireguardMeshState::Starting {
            if let Some(failure) = temps_network::control_plane::last_setup_failure() {
                reason = Some(format!(
                    "The control plane could not bring up its end of the mesh: {failure}. It \
                     retries on its own once the cluster network settings change."
                ));
            }
        }

        let control_plane = match (&settings, &published) {
            (Some(settings), Some(published)) => Some(WireguardMeshControlPlaneEntry {
                address: settings.control_plane_address().to_string(),
                endpoint: published.endpoint.clone(),
                endpoint_is_private: published
                    .endpoint
                    .as_deref()
                    .is_some_and(is_private_node_address),
            }),
            _ => None,
        };

        let (peers, handshake_error) = if state == WireguardMeshState::Ready {
            match tokio::task::spawn_blocking(temps_network::mesh::peer_status).await {
                Ok(Ok(peers)) => (Some(peers), None),
                Ok(Err(error)) => (None, Some(error.to_string())),
                Err(error) => (None, Some(format!("reading handshakes failed: {error}"))),
            }
        } else {
            (None, None)
        };

        let now = SystemTime::now();
        let (hub, links) = if state == WireguardMeshState::Ready {
            self.links_view(peers.as_deref()).await?
        } else {
            (None, Vec::new())
        };
        let nodes = self
            .node_service
            .list_all()
            .await?
            .into_iter()
            .map(|node| {
                let peer = peers.as_ref().and_then(|peers| {
                    node.mesh_wg_public_key
                        .as_deref()
                        .and_then(|key| peers.iter().find(|peer| peer.public_key == key))
                });
                let connection = node_connection(state, &node, peer, peers.is_some(), now);
                let mut checks = node_checks(
                    connection,
                    &node,
                    control_plane
                        .as_ref()
                        .and_then(|entry| entry.endpoint.as_deref()),
                    listen_port,
                    handshake_error.as_deref(),
                    now,
                );
                checks.extend(links_check(node.id, &links, hub.as_ref()));
                WireguardMeshNodeStatus {
                    node_id: node.id,
                    registered_on_private_network: is_private_node_address(&node.private_address),
                    data_address: node.data_address().to_string(),
                    name: node.name,
                    node_status: node.status,
                    registered_address: node.private_address,
                    mesh_address: node.mesh_wg_address,
                    endpoint: node.mesh_wg_endpoint,
                    connection,
                    last_handshake_at: peer
                        .and_then(|peer| peer.last_handshake)
                        .map(|at| chrono::DateTime::<chrono::Utc>::from(at).to_rfc3339()),
                    rx_bytes: peer.map(|peer| peer.rx_bytes),
                    tx_bytes: peer.map(|peer| peer.tx_bytes),
                    checks,
                }
            })
            .collect();

        let blocker = if settings.is_none() {
            enable_blocker()
        } else {
            None
        };
        // Onboarding only: a settings read failure leaves the join URL out
        // rather than failing the whole status.
        let join_url = self
            .config_service
            .get_external_url()
            .await
            .ok()
            .flatten()
            .map(|url| url.trim_end_matches('/').to_string())
            .filter(|url| !url.is_empty());

        Ok(WireguardMeshStatusResponse {
            state,
            reason,
            cidr: settings.as_ref().map(|settings| settings.cidr.to_string()),
            listen_port,
            control_plane,
            can_enable: settings.is_none() && blocker.is_none(),
            enable_blocker: blocker,
            enable_command: ENABLE_MESH_COMMAND.to_string(),
            join_url,
            handshake_error,
            nodes,
            hub,
            links,
        })
    }

    /// Turn the mesh on (idempotent). Refused on a server that cannot bring
    /// up the control plane's end, unless the mesh is already on.
    pub async fn enable(&self, request: EnableMesh) -> Result<MeshSettings, WireguardMeshError> {
        let db = self.db.as_ref();
        let already_on = temps_network::mesh::load_settings(db)
            .await
            .map_err(mesh("read the mesh settings"))?
            .is_some();
        if !already_on {
            if let Some(reason) = enable_blocker() {
                return Err(WireguardMeshError::UnavailableHere { reason });
            }
        }
        temps_network::mesh::enable(
            db,
            request.cidr.as_deref().map(str::trim),
            request.listen_port,
            request.node_api_port,
        )
        .await
        .map_err(mesh("turn the mesh on"))
    }

    /// Make `hub` the mesh hub, or remove it (`None`).
    pub async fn set_hub(&self, hub: Option<Hub>) -> Result<(), WireguardMeshError> {
        temps_network::mesh_links::set_hub(self.db.as_ref(), hub)
            .await
            .map_err(mesh("set the mesh hub"))
    }

    /// The hub and every pair's state, from what the members report and the
    /// control plane's own handshakes.
    async fn links_view(
        &self,
        control_plane_peers: Option<&[MeshPeerStatus]>,
    ) -> Result<(Option<WireguardMeshHub>, Vec<WireguardMeshLink>), WireguardMeshError> {
        let db = self.db.as_ref();
        let handshakes = control_plane_peers
            .unwrap_or_default()
            .iter()
            .filter_map(|peer| {
                Some((
                    peer.public_key.clone(),
                    chrono::DateTime::<chrono::Utc>::from(peer.last_handshake?),
                ))
            })
            .collect();
        let members = ml::members(db, &handshakes)
            .await
            .map_err(mesh("read the mesh members"))?;
        let hub = ml::load_hub(db).await.map_err(mesh("read the mesh hub"))?;
        let links = ml::load_links(db)
            .await
            .map_err(mesh("read the mesh links"))?;
        let now = chrono::Utc::now();

        let hub_member = hub.and_then(|hub| members.iter().find(|member| member.is(hub)));
        let hub_view = hub.map(|hub| WireguardMeshHub {
            target: match hub {
                Hub::ControlPlane => WireguardMeshHubTarget::ControlPlane,
                Hub::Node(node_id) => WireguardMeshHubTarget::Node { node_id },
            },
            name: hub_member
                .map(|member| member.name.clone())
                .unwrap_or_else(|| "a node that is not on the mesh".to_string()),
        });

        let mut view = Vec::new();
        for (index, a) in members.iter().enumerate() {
            for b in &members[index + 1..] {
                let (key_a, key_b) = ml::pair(&a.key, &b.key);
                let link = links.get(&(key_a.to_string(), key_b.to_string()));
                let state = ml::state(link, a, b, now);
                let detail = match state {
                    LinkState::Direct => None,
                    LinkState::ViaHub => Some(match hub_member {
                        Some(hub) => format!("relayed by {}", hub.name),
                        None => "relayed by a hub that is no longer on the mesh".to_string(),
                    }),
                    LinkState::Connecting => Some(if hub.is_some() {
                        "trying the direct path; if it does not come up, the hub carries it"
                            .to_string()
                    } else {
                        "trying the direct path".to_string()
                    }),
                    LinkState::Unreachable => Some(unreachable_detail(a, b, &members, hub, now)),
                };
                view.push(WireguardMeshLink {
                    a: a.name.clone(),
                    b: b.name.clone(),
                    a_node_id: a.node_id,
                    b_node_id: b.node_id,
                    state: match state {
                        LinkState::Direct => WireguardMeshLinkState::Direct,
                        LinkState::ViaHub => WireguardMeshLinkState::ViaHub,
                        LinkState::Connecting => WireguardMeshLinkState::Connecting,
                        LinkState::Unreachable => WireguardMeshLinkState::Unreachable,
                    },
                    last_handshake_at: ml::last_handshake(a, b).map(|at| at.to_rfc3339()),
                    detail,
                });
            }
        }
        Ok((hub_view, view))
    }
}

/// What prevents this server from bringing up the control plane's end of
/// the mesh, if anything.
pub fn enable_blocker() -> Option<String> {
    (!cfg!(target_os = "linux")).then(|| {
        "The WireGuard mesh needs kernel WireGuard, so the control plane must run on Linux."
            .to_string()
    })
}

/// Classify a node's standing on the mesh from its row and the control
/// plane's kernel peer entry for it.
fn node_connection(
    state: WireguardMeshState,
    node: &nodes::Model,
    peer: Option<&MeshPeerStatus>,
    handshakes_known: bool,
    now: SystemTime,
) -> WireguardMeshNodeConnection {
    if state == WireguardMeshState::Disabled {
        return WireguardMeshNodeConnection::MeshOff;
    }
    if node.mesh_wg_public_key.is_none() || node.mesh_wg_address.is_none() {
        return WireguardMeshNodeConnection::NotRegistered;
    }
    if state == WireguardMeshState::Starting {
        return WireguardMeshNodeConnection::WaitingForControlPlane;
    }
    if !handshakes_known {
        return WireguardMeshNodeConnection::Unknown;
    }
    match peer.and_then(|peer| peer.last_handshake) {
        None => WireguardMeshNodeConnection::NeverConnected,
        Some(at) => match now.duration_since(at) {
            Ok(age) if age >= LIVE_HANDSHAKE => WireguardMeshNodeConnection::Stale,
            // A handshake timestamped slightly ahead (clock skew) is fresh.
            _ => WireguardMeshNodeConnection::Connected,
        },
    }
}

/// What the control plane can tell about `node`'s mesh link.
fn node_checks(
    connection: WireguardMeshNodeConnection,
    node: &nodes::Model,
    control_plane_endpoint: Option<&str>,
    listen_port: u16,
    handshake_error: Option<&str>,
    now: SystemTime,
) -> Vec<WireguardMeshCheck> {
    use WireguardMeshCheckStatus::*;
    use WireguardMeshNodeConnection as C;

    let check = |label: &str, status, detail: String, fix: Option<String>| WireguardMeshCheck {
        label: label.to_string(),
        status,
        detail,
        fix,
    };
    let name = &node.name;
    let node_doctor = format!(
        "Run `temps doctor mesh` on {name}: it checks the node's end and says what to fix."
    );
    let path_fix = || {
        let node_endpoint = node.mesh_wg_endpoint.as_deref();
        Some(match (node_endpoint, control_plane_endpoint) {
            (Some(node_endpoint), Some(own)) => format!(
                "Open UDP {listen_port} inbound on {name} (it is dialed at {node_endpoint}) or on this server (dialed at {own}): either direction is enough. {node_doctor}"
            ),
            (Some(node_endpoint), None) => format!(
                "This server dials {name} at {node_endpoint}: open UDP {port} inbound on {name} (its provider's firewall and host firewall). {node_doctor}",
                port = node_endpoint
                    .rsplit_once(':')
                    .map(|(_, port)| port)
                    .unwrap_or("51820")
            ),
            (None, Some(own)) => format!(
                "{name} dials this server at {own}: open UDP {listen_port} inbound here. {node_doctor}"
            ),
            (None, None) => format!(
                "Neither this server nor {name} can be dialed. Give one a reachable address: `temps agent --wg-endpoint` on {name}, or `--private-address` for `temps serve`."
            ),
        })
    };

    // `None`: the node has no mesh key, so there is no handshake to judge.
    let handshake = match connection {
        C::MeshOff => return Vec::new(),
        C::NotRegistered => None,
        C::Connected => Some(check("Handshake", Pass, "live".to_string(), None)),
        C::Stale => Some(check(
            "Handshake",
            Warn,
            "none in the last three minutes: the tunnel is down".to_string(),
            path_fix(),
        )),
        C::NeverConnected => Some(check(
            "Handshake",
            Fail,
            "never handshook with this server".to_string(),
            path_fix(),
        )),
        C::WaitingForControlPlane => Some(check(
            "Handshake",
            Info,
            "waiting for this server to bring its end up".to_string(),
            None,
        )),
        C::Unknown => Some(check(
            "Handshake",
            Warn,
            format!(
                "this server could not read its handshakes: {}",
                handshake_error.unwrap_or("unknown error")
            ),
            Some("Check that `temps serve` runs as root or with CAP_NET_ADMIN.".to_string()),
        )),
    };

    let heartbeat_age = node
        .last_heartbeat
        .and_then(|at| now.duration_since(SystemTime::from(at)).ok());
    let mut checks = vec![if node.status == "active" {
        check(
            "Agent",
            Pass,
            match heartbeat_age {
                Some(age) => format!("reporting (heartbeat {} ago)", describe_age(age)),
                None => "reporting".to_string(),
            },
            None,
        )
    } else {
        check(
            "Agent",
            Fail,
            match heartbeat_age {
                Some(age) => format!("{} (last heartbeat {} ago)", node.status, describe_age(age)),
                None => format!("{} (no heartbeat yet)", node.status),
            },
            Some(format!(
                "Make sure `temps agent` runs on {name}. {node_doctor}"
            )),
        )
    }];

    match handshake {
        None => checks.push(check(
            "Mesh key",
            Fail,
            "the node's agent has not registered a mesh key".to_string(),
            Some(format!(
                "Run the current `temps agent` on {name} (older versions do not join the mesh). {node_doctor}"
            )),
        )),
        Some(handshake) => {
            checks.push(check(
                "Mesh key",
                Pass,
                format!(
                    "registered at {}",
                    node.mesh_wg_address.as_deref().unwrap_or("?")
                ),
                None,
            ));
            checks.push(handshake);
        }
    }
    checks
}

fn describe_age(age: std::time::Duration) -> String {
    let secs = age.as_secs();
    if secs < 120 {
        format!("{secs}s")
    } else if secs < 7200 {
        format!("{}m", secs / 60)
    } else if secs < 172_800 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86_400)
    }
}

fn state_of(
    settings: Option<&MeshSettings>,
    published: bool,
) -> (WireguardMeshState, Option<String>) {
    match (settings, published) {
        (None, _) => (
            WireguardMeshState::Disabled,
            Some(
                "Nodes must share a private network with the control plane. Enable the \
                 WireGuard mesh to join nodes that only share the internet."
                    .to_string(),
            ),
        ),
        (Some(_), false) => (
            WireguardMeshState::Starting,
            Some(
                "Enabled; waiting for `temps serve` to bring up the control plane's end, \
                 usually within a minute. If it stays here, look for WireGuard errors in the \
                 `temps serve` logs."
                    .to_string(),
            ),
        ),
        (Some(_), true) => (WireguardMeshState::Ready, None),
    }
}

/// Why a pair stays disconnected, and what fixes it.
fn unreachable_detail(
    a: &Member,
    b: &Member,
    members: &[Member],
    hub: Option<Hub>,
    now: chrono::DateTime<chrono::Utc>,
) -> String {
    let (a_name, b_name) = (&a.name, &b.name);
    let candidates: Vec<&str> = members
        .iter()
        .filter(|member| ml::relays_between(member, a, b, now))
        .map(|member| member.name.as_str())
        .collect();
    let reach = if candidates.is_empty() {
        "No member reaches both yet: the hub needs an address both can dial.".to_string()
    } else {
        format!(
            "{} reach{} both.",
            candidates.join(", "),
            if candidates.len() == 1 { "es" } else { "" }
        )
    };
    let example = candidates.first().copied().unwrap_or("<member>");
    let Some(hub) = hub else {
        return format!(
            "{a_name} and {b_name} cannot reach each other directly (neither can dial the other). \
             Set a hub to relay between them: Worker Nodes → Mesh hub, or `bunx @temps-sdk/cli \
             nodes mesh hub set {example}`. {reach}"
        );
    };
    if let Some((hub_end, other)) = [(a, b), (b, a)].into_iter().find(|(end, _)| end.is(hub)) {
        return format!(
            "{} is the hub, so it cannot relay its own link to {}: give one of them an address \
             the other can dial, or make another member the hub. {reach}",
            hub_end.name, other.name
        );
    }
    let Some(hub_member) = members.iter().find(|member| member.is(hub)) else {
        return format!("The hub is no longer on the mesh: choose another. {reach}");
    };
    // A hub relays only while it is fresh (`mesh_links`): a node hub that
    // stopped reporting, or a control plane whose relay setup failed, sends
    // its pairs back to direct.
    if hub_member.node_id.is_none() && !hub_member.fresh(now) {
        return format!(
            "{a_name} and {b_name} cannot reach each other, and the hub, the control plane, \
             could not set up relaying, so their pair went back to direct. Its log says why \
             (\"could not update WireGuard mesh relaying\"); fix that, or choose another hub: \
             `bunx @temps-sdk/cli nodes mesh hub set {example}`. {reach}"
        );
    }
    if hub_member.node_id.is_some() && !hub_member.fresh(now) {
        return format!(
            "{a_name} and {b_name} cannot reach each other, and the hub, {hub}, has not reported \
             its handshakes for over {fresh} s, so its relayed pairs went back to direct. Check \
             that `temps agent` runs on {hub} (its log says why relaying stopped), or choose \
             another hub: `bunx @temps-sdk/cli nodes mesh hub set {example}`. {reach}",
            hub = hub_member.name,
            fresh = ml::FRESH_REPORT.as_secs(),
        );
    }
    let unreached: Vec<&str> = [a, b]
        .into_iter()
        .filter(|member| !ml::is_live(hub_member, member, now))
        .map(|member| member.name.as_str())
        .collect();
    if !unreached.is_empty() {
        return format!(
            "{a_name} and {b_name} cannot reach each other, and the hub, {}, has no working \
             link to {} either, so it cannot relay between them. Choose a hub both reach: \
             `bunx @temps-sdk/cli nodes mesh hub set {example}`. {reach}",
            hub_member.name,
            unreached.join(" or ")
        );
    }
    let silent: Vec<&str> = [a, b]
        .into_iter()
        .filter(|member| !member.fresh(now))
        .map(|member| member.name.as_str())
        .collect();
    if silent.is_empty() {
        return format!(
            "{a_name} and {b_name} never handshook; the hub takes them over on its next check, \
             within a minute."
        );
    }
    format!(
        "{a_name} and {b_name} never handshook, but the hub only takes over once both report \
         their handshakes, and {} {} not: make sure the current `temps agent` runs there.",
        silent.join(" and "),
        if silent.len() == 1 { "does" } else { "do" }
    )
}

/// A node's links to the other members, in one check.
fn links_check(
    node_id: i32,
    links: &[WireguardMeshLink],
    hub: Option<&WireguardMeshHub>,
) -> Option<WireguardMeshCheck> {
    let mine: Vec<(&str, &WireguardMeshLink)> = links
        .iter()
        .filter_map(|link| {
            if link.a_node_id == Some(node_id) {
                Some((link.b.as_str(), link))
            } else if link.b_node_id == Some(node_id) {
                Some((link.a.as_str(), link))
            } else {
                None
            }
        })
        .collect();
    if mine.is_empty() {
        return None;
    }
    let with = |state: WireguardMeshLinkState| -> Vec<&str> {
        mine.iter()
            .filter(|(_, link)| link.state == state)
            .map(|(other, _)| *other)
            .collect()
    };
    let unreachable = with(WireguardMeshLinkState::Unreachable);
    let relayed = with(WireguardMeshLinkState::ViaHub);
    let connecting = with(WireguardMeshLinkState::Connecting);
    Some(if !unreachable.is_empty() {
        let fix = mine
            .iter()
            .find(|(_, link)| link.state == WireguardMeshLinkState::Unreachable)
            .and_then(|(_, link)| link.detail.clone());
        WireguardMeshCheck {
            label: "Links".into(),
            status: WireguardMeshCheckStatus::Fail,
            detail: format!("cannot reach {}", unreachable.join(", ")),
            fix,
        }
    } else if !relayed.is_empty() {
        WireguardMeshCheck {
            label: "Links".into(),
            status: WireguardMeshCheckStatus::Info,
            detail: format!(
                "reaches {} through the hub{}",
                relayed.join(", "),
                hub.map(|hub| format!(" ({})", hub.name))
                    .unwrap_or_default()
            ),
            fix: None,
        }
    } else if !connecting.is_empty() {
        WireguardMeshCheck {
            label: "Links".into(),
            status: WireguardMeshCheckStatus::Info,
            detail: format!("connecting to {}", connecting.join(", ")),
            fix: None,
        }
    } else {
        WireguardMeshCheck {
            label: "Links".into(),
            status: WireguardMeshCheckStatus::Pass,
            detail: "reaches every member directly".into(),
            fix: None,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::time::Duration;

    fn node(registered: bool) -> nodes::Model {
        nodes::Model {
            id: 1,
            name: "worker-1".into(),
            token_hash: String::new(),
            token_encrypted: None,
            address: "https://203.0.113.10:3100".into(),
            private_address: "203.0.113.10".into(),
            public_endpoint: None,
            wg_public_key: None,
            role: "worker".into(),
            status: "active".into(),
            labels: serde_json::json!({}),
            capacity: serde_json::json!({}),
            last_heartbeat: None,
            edge_public_key: None,
            compute_cidr: None,
            architecture: None,
            underlay_address: None,
            mesh_wg_public_key: registered.then(|| "key".to_string()),
            mesh_wg_endpoint: registered.then(|| "203.0.113.10:51820".to_string()),
            mesh_wg_address: registered.then(|| "10.201.0.2".to_string()),
            dns_resolver_running: None,
            dns_resolver_tasks_alive: None,
            dns_resolver_last_sync_at: None,
            dns_resolver_consecutive_failures: 0,
            dns_resolver_last_error: None,
            dns_resolver_record_count: None,
            failover_at: None,
            public_ingress_enabled: false,
            public_ingress_running: None,
            public_ingress_last_error: None,
            public_ingress_certificate_count: None,
            public_ingress_route_count: None,
            public_ingress_unsupported_route_count: None,
            public_ingress_unsupported_reasons: serde_json::json!([]),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    fn peer(last_handshake: Option<SystemTime>) -> MeshPeerStatus {
        MeshPeerStatus {
            public_key: "key".into(),
            endpoint: None,
            last_handshake,
            rx_bytes: 1,
            tx_bytes: 2,
        }
    }

    fn status(checks: &[WireguardMeshCheck], label: &str) -> WireguardMeshCheckStatus {
        checks
            .iter()
            .find(|check| check.label == label)
            .unwrap_or_else(|| panic!("no {label} check in {checks:?}"))
            .status
    }

    #[test]
    fn node_checks_say_which_side_to_open_for_a_missing_handshake() {
        use WireguardMeshCheckStatus::*;
        use WireguardMeshNodeConnection::*;
        let now = SystemTime::now();
        let registered = node(true);

        // The control plane has no endpoint: it dials the node, so the node's
        // port must be open.
        let checks = node_checks(NeverConnected, &registered, None, 51820, None, now);
        assert_eq!(status(&checks, "Handshake"), Fail);
        let fix = checks[2].fix.as_deref().unwrap();
        assert!(
            fix.contains("This server dials worker-1 at 203.0.113.10:51820"),
            "{fix}"
        );
        assert!(fix.contains("temps doctor mesh"), "{fix}");

        // A node with no endpoint dials the control plane.
        let mut dials_in = node(true);
        dials_in.mesh_wg_endpoint = None;
        let checks = node_checks(
            Stale,
            &dials_in,
            Some("198.51.100.1:51820"),
            51820,
            None,
            now,
        );
        assert_eq!(status(&checks, "Handshake"), Warn);
        assert!(checks[2]
            .fix
            .as_deref()
            .unwrap()
            .contains("worker-1 dials this server at 198.51.100.1:51820"));

        // Neither end dialable.
        let checks = node_checks(NeverConnected, &dials_in, None, 51820, None, now);
        assert!(checks[2].fix.as_deref().unwrap().contains("Neither"));

        let checks = node_checks(Connected, &registered, None, 51820, None, now);
        assert!(
            checks.iter().all(|check| check.status == Pass),
            "{checks:?}"
        );
    }

    #[test]
    fn node_checks_point_an_offline_or_unregistered_node_at_its_own_doctor() {
        use WireguardMeshCheckStatus::*;
        use WireguardMeshNodeConnection::*;
        let now = SystemTime::now();
        let mut offline = node(false);
        offline.status = "offline".into();
        let checks = node_checks(NotRegistered, &offline, None, 51820, None, now);
        assert_eq!(status(&checks, "Agent"), Fail);
        assert_eq!(status(&checks, "Mesh key"), Fail);
        assert!(!checks.iter().any(|check| check.label == "Handshake"));
        assert!(checks[0].detail.contains("no heartbeat yet"));
        assert!(node_checks(MeshOff, &offline, None, 51820, None, now).is_empty());
    }

    #[test]
    fn node_checks_cover_waiting_and_unreadable_handshakes() {
        use WireguardMeshCheckStatus::*;
        use WireguardMeshNodeConnection::*;
        let now = SystemTime::now();
        let registered = node(true);

        let checks = node_checks(WaitingForControlPlane, &registered, None, 51820, None, now);
        assert_eq!(status(&checks, "Handshake"), Info);
        assert_eq!(status(&checks, "Mesh key"), Pass);

        let checks = node_checks(
            Unknown,
            &registered,
            None,
            51820,
            Some("permission denied"),
            now,
        );
        assert_eq!(status(&checks, "Handshake"), Warn);
        assert!(checks[2].detail.contains("permission denied"));
        assert!(checks[2].fix.as_deref().unwrap().contains("CAP_NET_ADMIN"));
    }

    #[test]
    fn a_node_is_connected_only_after_a_recent_handshake() {
        let now = SystemTime::now();
        let registered = node(true);
        let recent = peer(Some(now - Duration::from_secs(30)));
        let old = peer(Some(now - LIVE_HANDSHAKE - Duration::from_secs(1)));
        let never = peer(None);
        use WireguardMeshNodeConnection::*;
        use WireguardMeshState::Ready;
        assert_eq!(
            node_connection(Ready, &registered, Some(&recent), true, now),
            Connected
        );
        assert_eq!(
            node_connection(Ready, &registered, Some(&old), true, now),
            Stale
        );
        assert_eq!(
            node_connection(Ready, &registered, Some(&never), true, now),
            NeverConnected
        );
        // Registered but not yet a peer on the control plane.
        assert_eq!(
            node_connection(Ready, &registered, None, true, now),
            NeverConnected
        );
    }

    #[test]
    fn node_connection_says_why_there_is_no_handshake() {
        let now = SystemTime::now();
        use WireguardMeshNodeConnection::*;
        use WireguardMeshState::{Disabled, Ready, Starting};
        assert_eq!(
            node_connection(Disabled, &node(true), None, true, now),
            MeshOff
        );
        assert_eq!(
            node_connection(Ready, &node(false), None, true, now),
            NotRegistered
        );
        assert_eq!(
            node_connection(Starting, &node(true), None, false, now),
            WaitingForControlPlane
        );
        assert_eq!(
            node_connection(Ready, &node(true), None, false, now),
            Unknown
        );
    }

    #[test]
    fn the_mesh_is_ready_only_once_the_control_plane_published_its_end() {
        let settings = MeshSettings {
            cidr: "10.201.0.0/16".parse().unwrap(),
            port: 51820,
            node_api_port: 51820,
        };
        let (off, off_reason) = state_of(None, false);
        assert_eq!(off, WireguardMeshState::Disabled);
        assert!(off_reason.unwrap().contains("Enable the WireGuard mesh"));
        let (starting, starting_reason) = state_of(Some(&settings), false);
        assert_eq!(starting, WireguardMeshState::Starting);
        assert!(starting_reason.unwrap().contains("temps serve"));
        assert_eq!(
            state_of(Some(&settings), true),
            (WireguardMeshState::Ready, None)
        );
    }

    fn link(b: &str, b_node_id: i32, state: WireguardMeshLinkState) -> WireguardMeshLink {
        WireguardMeshLink {
            a: "worker-1".into(),
            b: b.into(),
            a_node_id: Some(1),
            b_node_id: Some(b_node_id),
            state,
            last_handshake_at: None,
            detail: Some(format!("fix for {b}")),
        }
    }

    #[test]
    fn a_node_links_check_leads_with_what_is_broken() {
        use WireguardMeshLinkState::*;
        let hub = WireguardMeshHub {
            target: WireguardMeshHubTarget::ControlPlane,
            name: "control-plane".into(),
        };
        let links = [
            link("worker-2", 2, Direct),
            link("worker-3", 3, ViaHub),
            link("worker-4", 4, Unreachable),
        ];

        let check = links_check(1, &links, Some(&hub)).unwrap();
        assert_eq!(check.status, WireguardMeshCheckStatus::Fail);
        assert_eq!(check.detail, "cannot reach worker-4");
        assert_eq!(check.fix.as_deref(), Some("fix for worker-4"));

        let check = links_check(1, &links[..2], Some(&hub)).unwrap();
        assert_eq!(check.status, WireguardMeshCheckStatus::Info);
        assert_eq!(
            check.detail,
            "reaches worker-3 through the hub (control-plane)"
        );

        let check = links_check(1, &links[..1], None).unwrap();
        assert_eq!(check.status, WireguardMeshCheckStatus::Pass);

        let check = links_check(1, &[link("worker-5", 5, Connecting)], None).unwrap();
        assert_eq!(check.detail, "connecting to worker-5");

        // Seen from the other end of the pair.
        let check = links_check(4, &links, Some(&hub)).unwrap();
        assert_eq!(check.detail, "cannot reach worker-1");

        assert!(
            links_check(9, &links, None).is_none(),
            "not a member of any pair"
        );
    }

    #[test]
    fn hub_targets_name_the_hub() {
        assert_eq!(WireguardMeshHubTarget::None.hub(), None);
        assert_eq!(
            WireguardMeshHubTarget::ControlPlane.hub(),
            Some(Hub::ControlPlane)
        );
        assert_eq!(
            WireguardMeshHubTarget::Node { node_id: 7 }.hub(),
            Some(Hub::Node(7))
        );
    }

    // ── unreachable_detail ──────────────────────────────────────────────

    /// A node member that reported `handshakes` (peer key -> seconds ago),
    /// or did not report at all (`None`).
    fn member(
        node_id: i32,
        handshakes: Option<&[(&str, i64)]>,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Member {
        Member {
            key: format!("key-{node_id}"),
            name: format!("worker-{node_id}"),
            node_id: Some(node_id),
            address: std::net::Ipv4Addr::new(10, 201, 0, node_id as u8),
            endpoint: None,
            handshakes: handshakes.map(|handshakes| {
                handshakes
                    .iter()
                    .map(|(key, ago)| (key.to_string(), now - chrono::Duration::seconds(*ago)))
                    .collect::<HashMap<_, _>>()
            }),
            reported_at: handshakes.map(|_| now),
        }
    }

    /// worker-1 and worker-2 never handshook; worker-3 (when present)
    /// reaches both.
    fn pair_and_relay(now: chrono::DateTime<chrono::Utc>) -> (Member, Member, Member) {
        (
            member(1, Some(&[]), now),
            member(2, Some(&[]), now),
            member(3, Some(&[("key-1", 10), ("key-2", 10)]), now),
        )
    }

    #[test]
    fn unreachable_without_a_hub_names_the_members_that_could_relay() {
        let now = chrono::Utc::now();
        let (a, b, relay) = pair_and_relay(now);
        let members = [a.clone(), b.clone(), relay];
        let detail = unreachable_detail(&a, &b, &members, None, now);
        assert!(detail.contains("Set a hub"), "{detail}");
        assert!(detail.contains("nodes mesh hub set worker-3"), "{detail}");
        assert!(detail.ends_with("worker-3 reaches both."), "{detail}");
    }

    #[test]
    fn unreachable_without_a_hub_or_a_candidate_says_none_reaches_both() {
        let now = chrono::Utc::now();
        let (a, b, _) = pair_and_relay(now);
        let members = [a.clone(), b.clone()];
        let detail = unreachable_detail(&a, &b, &members, None, now);
        assert!(detail.contains("Set a hub"), "{detail}");
        assert!(detail.contains("hub set <member>"), "{detail}");
        assert!(detail.contains("No member reaches both yet"), "{detail}");
    }

    #[test]
    fn unreachable_when_the_hub_is_one_of_the_pair() {
        let now = chrono::Utc::now();
        let (a, b, relay) = pair_and_relay(now);
        let members = [a.clone(), b.clone(), relay];
        let detail = unreachable_detail(&a, &b, &members, Some(Hub::Node(2)), now);
        assert!(
            detail.starts_with("worker-2 is the hub, so it cannot relay its own link to worker-1"),
            "{detail}"
        );
    }

    #[test]
    fn unreachable_when_a_node_hub_stopped_reporting() {
        let now = chrono::Utc::now();
        let (a, b, _) = pair_and_relay(now);
        // worker-3 still has recent handshakes with both, but its last
        // report is two minutes old.
        let mut silent_hub = member(3, Some(&[("key-1", 10), ("key-2", 10)]), now);
        silent_hub.reported_at = Some(now - chrono::Duration::seconds(120));
        let members = [a.clone(), b.clone(), silent_hub];
        let detail = unreachable_detail(&a, &b, &members, Some(Hub::Node(3)), now);
        assert!(
            detail.contains("the hub, worker-3, has not reported its handshakes for over 90 s"),
            "{detail}"
        );
        assert!(
            detail.contains("`temps agent` runs on worker-3"),
            "{detail}"
        );
    }

    #[test]
    fn unreachable_when_the_control_plane_hub_cannot_relay() {
        let now = chrono::Utc::now();
        let (a, b, _) = pair_and_relay(now);
        let mut control_plane = member(1000, Some(&[("key-1", 10), ("key-2", 10)]), now);
        control_plane.node_id = None;
        control_plane.name = "control-plane".into();
        // `members` leaves the control plane unreported when it is the hub
        // and its relay setup failed.
        control_plane.reported_at = None;
        let members = [a.clone(), b.clone(), control_plane];
        let detail = unreachable_detail(&a, &b, &members, Some(Hub::ControlPlane), now);
        assert!(
            detail.contains("the hub, the control plane, could not set up relaying"),
            "{detail}"
        );
    }

    #[test]
    fn unreachable_when_the_hub_left_the_mesh() {
        let now = chrono::Utc::now();
        let (a, b, relay) = pair_and_relay(now);
        let members = [a.clone(), b.clone(), relay];
        let detail = unreachable_detail(&a, &b, &members, Some(Hub::Node(99)), now);
        assert!(
            detail.starts_with("The hub is no longer on the mesh: choose another."),
            "{detail}"
        );
    }

    #[test]
    fn unreachable_when_the_hub_does_not_reach_one_side() {
        let now = chrono::Utc::now();
        let (a, b, _) = pair_and_relay(now);
        // worker-3 reaches worker-1 only; its handshake with worker-2 is old.
        let half_hub = member(3, Some(&[("key-1", 10), ("key-2", 3600)]), now);
        let members = [a.clone(), b.clone(), half_hub];
        let detail = unreachable_detail(&a, &b, &members, Some(Hub::Node(3)), now);
        assert!(
            detail.contains("the hub, worker-3, has no working link to worker-2 either"),
            "{detail}"
        );
        assert!(detail.contains("No member reaches both yet"), "{detail}");
    }

    #[test]
    fn unreachable_with_both_reporting_waits_for_the_next_check() {
        let now = chrono::Utc::now();
        let (a, b, relay) = pair_and_relay(now);
        let members = [a.clone(), b.clone(), relay];
        let detail = unreachable_detail(&a, &b, &members, Some(Hub::Node(3)), now);
        assert_eq!(
            detail,
            "worker-1 and worker-2 never handshook; the hub takes them over on its next \
             check, within a minute."
        );
    }

    #[test]
    fn unreachable_with_a_silent_member_says_which_agent_to_run() {
        let now = chrono::Utc::now();
        let (a, _, relay) = pair_and_relay(now);
        // worker-2 never reported (an older agent): the hub reaches it, but
        // cannot tell that the direct link is down.
        let silent = member(2, None, now);
        let members = [a.clone(), silent.clone(), relay];
        let detail = unreachable_detail(&a, &silent, &members, Some(Hub::Node(3)), now);
        assert!(
            detail
                .contains("and worker-2 does not: make sure the current `temps agent` runs there"),
            "{detail}"
        );
    }
}
