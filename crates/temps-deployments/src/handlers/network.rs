// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Network handlers — peer-list endpoint for worker nodes.
//!
//! `GET /internal/nodes/{node_id}/network/peers` is what each worker calls
//! after registration (and on a periodic timer) to learn:
//!   1. its own `compute_cidr` allocation
//!   2. the list of peer nodes it should reach via the overlay
//!
//! Authentication mirrors `node_heartbeat`: the worker presents the same
//! bearer token it registered with, sha256-hashed and compared in
//! constant time against `nodes.token_hash`.
//!
//! The endpoint never auto-allocates — that's the join handshake's job
//! and shouldn't happen on every poll. Workers without a `compute_cidr`
//! get `alloc: null` in the response and skip the multi-host bring-up.

use std::sync::Arc;

use axum::{
    extract::{ConnectInfo, Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use temps_core::problemdetails::{self, Problem};
use temps_network::allocator::{
    AllocatorError, ComputeNetworkAllocator, NodeAllocPersisted, PostgresAllocator,
};
use temps_network::config::Peer;
use temps_network::mesh::MeshError;
use tracing::{error, warn};
use utoipa::ToSchema;

use crate::handlers::audit::NodeMeshKeyChangedAudit;
use crate::handlers::nodes::NodeAppState;

/// Wire-format peer entry. Matches `temps_network::config::Peer` but
/// uses strings on the wire to keep the API stable across underlying
/// type evolution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PeerEntry {
    /// Stable v5 UUID derived from the database node id. Workers use
    /// this as the kernel-layer identifier when calling
    /// `NetworkManager::reconcile_peers`.
    pub node_id: String,
    /// Per-node CIDR (e.g. `"172.20.5.0/24"`).
    pub compute_cidr: String,
    /// Address the local node should use to reach this peer over the
    /// underlay (private VPC IP for same-DC, public IP for cross-DC).
    pub underlay_address: String,
}

impl From<Peer> for PeerEntry {
    fn from(p: Peer) -> Self {
        Self {
            node_id: p.node_id.to_string(),
            compute_cidr: p.compute_cidr.to_string(),
            underlay_address: p.underlay_address.to_string(),
        }
    }
}

/// Wire-format allocation. `null` in the JSON when the node hasn't been
/// allocated yet — workers should treat that as "single-host mode, do
/// not bring up the overlay".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AllocEntry {
    /// Stable v5 UUID derived from the database node id.
    pub node_id: String,
    pub compute_cidr: String,
    pub bridge_address: String,
    pub underlay_address: String,
}

impl From<NodeAllocPersisted> for AllocEntry {
    fn from(p: NodeAllocPersisted) -> Self {
        Self {
            node_id: p.external_id.to_string(),
            compute_cidr: p.compute_cidr.to_string(),
            bridge_address: p.bridge_address.to_string(),
            underlay_address: p.underlay_address.to_string(),
        }
    }
}

/// Cluster-wide pool which every node must use. This is intentionally sent
/// alongside the local allocation so operators and agents can detect stale or
/// independently configured nodes before routes are changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct NetworkPoolEntry {
    pub compute_pool_cidr: String,
    pub subnet_prefix_len: u8,
}

/// A WireGuard mesh peer on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct WireguardMeshPeerEntry {
    /// Node name, or `control-plane`. For status output only.
    pub name: String,
    pub public_key: String,
    /// `ip:port` to dial, or `null` when the peer has none (it dials us).
    pub endpoint: Option<String>,
    /// Peer's mesh address (its overlay underlay).
    pub address: String,
    /// On the hub's entry only (ADR 048 D4): the mesh addresses of the
    /// members this node reaches through the hub.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relayed: Vec<String>,
}

/// This node's registered mesh identity, as the control plane stored it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct WireguardMeshSelfEntry {
    pub public_key: String,
    pub endpoint: String,
    pub address: String,
}

/// Managed WireGuard mesh state for the calling node. Absent when the mesh
/// is off; the node then keeps its registered address as underlay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct WireguardMeshEntry {
    pub cidr: String,
    pub listen_port: u16,
    /// `null` until the node registers its key with
    /// `PUT /internal/nodes/{node_id}/network/wireguard`.
    #[serde(rename = "self")]
    pub self_entry: Option<WireguardMeshSelfEntry>,
    pub peers: Vec<WireguardMeshPeerEntry>,
    /// This node is the mesh hub (ADR 048 D4): it forwards traffic between
    /// members that cannot reach each other.
    #[serde(default)]
    pub hub: bool,
}

/// Body of `PUT /internal/nodes/{node_id}/network/wireguard/handshakes`: when
/// this node last completed a WireGuard handshake with each peer. Peers it
/// never handshook with are left out.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ReportWireguardHandshakesRequest {
    pub peers: Vec<WireguardHandshakeReport>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct WireguardHandshakeReport {
    pub public_key: String,
    /// Seconds since the last completed handshake, on the node's clock.
    pub seconds_since_handshake: u64,
}

/// A node reports at most this many peers.
const MAX_REPORTED_PEERS: usize = 4096;

/// Body of `PUT /internal/nodes/{node_id}/network/wireguard`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct RegisterWireguardMeshRequest {
    /// Base64 WireGuard public key. The private key never leaves the node.
    pub public_key: String,
    /// `ip:port` other nodes dial to reach this node's WireGuard socket.
    pub endpoint: String,
}

/// Response of `PUT /internal/nodes/{node_id}/network/wireguard`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RegisterWireguardMeshResponse {
    /// Mesh address assigned to this node; also its overlay underlay.
    pub address: String,
    pub prefix_len: u8,
    pub listen_port: u16,
}

/// Response body for `GET /internal/nodes/{node_id}/network/peers`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PeerListResponse {
    pub network: NetworkPoolEntry,
    /// Caller's own allocation, or `null` if multi-host networking has
    /// not been enabled for this node yet.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alloc: Option<AllocEntry>,
    /// All other nodes with a `compute_cidr` set, excluding the caller.
    pub peers: Vec<PeerEntry>,
    /// Whether the cluster-DNS resolver is enabled on this control plane
    /// (`AppSettings.cluster_dns.enabled`). Workers should start their
    /// per-node resolver and write `overlay_bridge_address` only when this
    /// is `true`. Always serialized (never `skip_serializing_if`) so older
    /// and newer version skew degrades to the safe default of `false`.
    pub cluster_dns_enabled: bool,
    /// Managed WireGuard mesh, when enabled on the cluster.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wireguard: Option<WireguardMeshEntry>,
}

/// `GET /internal/nodes/{node_id}/network/peers`
#[utoipa::path(
    tag = "Nodes",
    get,
    path = "/internal/nodes/{node_id}/network/peers",
    params(
        ("node_id" = i32, Path, description = "Node id, must match the bearer token's node")
    ),
    responses(
        (status = 200, description = "Peer list and self-allocation", body = PeerListResponse),
        (status = 401, description = "Missing or invalid bearer token"),
        (status = 404, description = "Node not found"),
        (status = 500, description = "Internal server error")
    )
)]
pub async fn list_peers(
    State(app_state): State<Arc<NodeAppState>>,
    headers: HeaderMap,
    Path(node_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    // ----- 1. Token auth (mirrors node_heartbeat) -----
    let node = authenticate_node(&app_state, &headers, node_id).await?;

    // ----- 2. Self-alloc + peers -----
    let allocator = PostgresAllocator::new(app_state.db.clone());
    let cluster_config = allocator.cluster_config().await.map_err(|error| {
        error!(node_id, "cluster network config failed: {error}");
        problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
            .with_title("Allocator Error")
            .with_detail(error.to_string())
    })?;
    if node.mesh_wg_address.is_some() && node.compute_cidr.is_none() {
        allocate_after_mesh_registration(&app_state, node_id).await;
    }
    let alloc = match allocator.get_alloc(node_id).await {
        Ok(a) => a.map(AllocEntry::from),
        Err(AllocatorError::NodeNotFound { .. }) => {
            // The node existed at step 1 but vanished — extremely rare
            // race; treat as 404 rather than 500.
            return Err(problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Node Not Found")
                .with_detail(format!("Node {} no longer exists", node_id)));
        }
        Err(e) => {
            error!(node_id, "get_alloc failed: {}", e);
            return Err(problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Allocator Error")
                .with_detail(e.to_string()));
        }
    };

    let peers = allocator
        .peer_list(node_id)
        .await
        .map_err(|e| {
            error!(node_id, "peer_list failed: {}", e);
            problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Allocator Error")
                .with_detail(e.to_string())
        })?
        .into_iter()
        .map(PeerEntry::from)
        .collect();

    // Reflect the cluster-DNS feature flag into the wire response so workers
    // can gate their per-node resolver consistently with the control plane.
    // Best-effort: when settings can't be read (transient DB hiccup), default
    // to `false` — the safe side that never accidentally enables DNS injection.
    let cluster_dns_enabled = match app_state.config_service.get_settings().await {
        Ok(settings) => settings.cluster_dns.enabled,
        Err(e) => {
            warn!(
                node_id,
                "could not read cluster_dns setting: {}; defaulting to disabled", e
            );
            false
        }
    };

    let wireguard = mesh_entry(&app_state, &node).await?;

    Ok(Json(PeerListResponse {
        network: NetworkPoolEntry {
            compute_pool_cidr: cluster_config.compute_pool_cidr.to_string(),
            subnet_prefix_len: cluster_config.subnet_prefix_len,
        },
        alloc,
        peers,
        cluster_dns_enabled,
        wireguard,
    }))
}

async fn mesh_entry(
    app_state: &NodeAppState,
    node: &temps_entities::nodes::Model,
) -> Result<Option<WireguardMeshEntry>, Problem> {
    let mesh_error = |error: MeshError| {
        error!(node_id = node.id, "WireGuard mesh state failed: {error}");
        problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
            .with_title("WireGuard Mesh Error")
            .with_detail("the control plane could not read the WireGuard mesh state; see its logs")
    };
    let Some(settings) = temps_network::mesh::settings_for_workers(app_state.db.as_ref())
        .await
        .map_err(mesh_error)?
    else {
        return Ok(None);
    };
    let peers = temps_network::mesh::peers(app_state.db.as_ref(), Some(node.id))
        .await
        .map_err(mesh_error)?
        .into_iter()
        .map(|named| WireguardMeshPeerEntry {
            name: named.name,
            public_key: named.peer.public_key,
            endpoint: named.peer.endpoint.map(|endpoint| endpoint.to_string()),
            address: named.peer.address.to_string(),
            relayed: named
                .peer
                .relayed
                .iter()
                .map(|address| address.to_string())
                .collect(),
        })
        .collect();
    let hub = temps_network::mesh_links::load_hub(app_state.db.as_ref())
        .await
        .map_err(mesh_error)?
        == Some(temps_network::mesh_links::Hub::Node(node.id));
    let self_entry = match (
        &node.mesh_wg_public_key,
        &node.mesh_wg_endpoint,
        &node.mesh_wg_address,
    ) {
        (Some(public_key), Some(endpoint), Some(address)) => Some(WireguardMeshSelfEntry {
            public_key: public_key.clone(),
            endpoint: endpoint.clone(),
            address: address.clone(),
        }),
        _ => None,
    };
    Ok(Some(WireguardMeshEntry {
        cidr: settings.cidr.to_string(),
        listen_port: settings.port,
        self_entry,
        peers,
        hub,
    }))
}

/// `PUT /internal/nodes/{node_id}/network/wireguard/handshakes`
///
/// Called by the node's own agent after each mesh sync. The control plane
/// routes pairs whose direct link never handshakes through the hub (ADR 048
/// D4).
#[utoipa::path(
    tag = "Nodes",
    put,
    path = "/internal/nodes/{node_id}/network/wireguard/handshakes",
    operation_id = "WireguardMeshHandshakesReport",
    params(
        ("node_id" = i32, Path, description = "Node id, must match the bearer token's node")
    ),
    request_body = ReportWireguardHandshakesRequest,
    responses(
        (status = 204, description = "Report recorded"),
        (status = 400, description = "Too many peers"),
        (status = 401, description = "Missing or invalid bearer token"),
        (status = 500, description = "Internal server error")
    )
)]
pub async fn report_mesh_handshakes(
    State(app_state): State<Arc<NodeAppState>>,
    headers: HeaderMap,
    Path(node_id): Path<i32>,
    Json(request): Json<ReportWireguardHandshakesRequest>,
) -> Result<impl IntoResponse, Problem> {
    authenticate_node(&app_state, &headers, node_id).await?;
    if request.peers.len() > MAX_REPORTED_PEERS {
        return Err(problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Too Many Peers")
            .with_detail(format!("report at most {MAX_REPORTED_PEERS} peers")));
    }
    let report = request
        .peers
        .into_iter()
        .map(|peer| (peer.public_key, peer.seconds_since_handshake))
        .collect();
    temps_network::mesh_links::record_report(&app_state.db, node_id, &report)
        .await
        .map_err(|error| {
            error!(node_id, "recording WireGuard handshakes failed: {error}");
            problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("WireGuard Mesh Error")
                .with_detail("the control plane could not record the handshakes; see its logs")
        })?;
    Ok(StatusCode::NO_CONTENT)
}

/// `PUT /internal/nodes/{node_id}/network/wireguard`
///
/// Called by the node's own agent. Stores its mesh public key and endpoint,
/// assigns a mesh address on first call, makes that address the node's
/// overlay underlay, and allocates the node's compute CIDR if the join could
/// not (a node that registered with a public address has no private underlay
/// until now).
#[utoipa::path(
    tag = "Nodes",
    put,
    path = "/internal/nodes/{node_id}/network/wireguard",
    operation_id = "WireguardMeshRegister",
    params(
        ("node_id" = i32, Path, description = "Node id, must match the bearer token's node")
    ),
    request_body = RegisterWireguardMeshRequest,
    responses(
        (status = 200, description = "Mesh address and port for this node", body = RegisterWireguardMeshResponse),
        (status = 400, description = "Invalid public key or endpoint"),
        (status = 401, description = "Missing or invalid bearer token"),
        (status = 404, description = "Node not found"),
        (status = 409, description = "Mesh disabled, key already in use, or pool exhausted"),
        (status = 500, description = "Internal server error")
    )
)]
pub async fn register_mesh(
    State(app_state): State<Arc<NodeAppState>>,
    // The peer that sent the registration, recorded when it re-keys the
    // node. Present in production (both listeners insert it) and injected
    // by `MockConnectInfo` in tests.
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    Path(node_id): Path<i32>,
    Json(request): Json<RegisterWireguardMeshRequest>,
) -> Result<impl IntoResponse, Problem> {
    // The node as it was before this call: its current mesh key, to tell a
    // re-key from the agent's routine re-registration.
    let node = authenticate_node(&app_state, &headers, node_id).await?;
    let endpoint = temps_network::mesh::parse_endpoint(&request.endpoint).map_err(|error| {
        problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Invalid WireGuard Endpoint")
            .with_detail(error.to_string())
    })?;
    let public_key = request.public_key.trim();
    let registration =
        temps_network::mesh::register_node(&app_state.db, node_id, public_key, endpoint)
            .await
            .map_err(|error| register_problem(node_id, error))?;

    if let Some(change) = mesh_key_change(&node, public_key, endpoint) {
        record_mesh_key_change(app_state.audit_service.as_ref(), change, peer).await;
    }

    // The mesh address is now the underlay; allocate the compute CIDR the
    // join skipped for a public registration address. `list_peers` retries
    // this on every poll, so a failure here heals on its own.
    allocate_after_mesh_registration(&app_state, node_id).await;

    Ok(Json(RegisterWireguardMeshResponse {
        address: registration.address.to_string(),
        prefix_len: registration.prefix_len,
        listen_port: registration.listen_port,
    }))
}

/// A node registering a mesh key other than the one on record.
#[derive(Debug, Clone, PartialEq, Eq)]
struct MeshKeyChange {
    node_id: i32,
    node_name: String,
    old_public_key: String,
    new_public_key: String,
    old_endpoint: Option<String>,
    new_endpoint: String,
}

/// The re-key `node` (as it was before the registration) undergoes by
/// registering `public_key` at `endpoint`: `None` on a first registration
/// or when the key is unchanged, so the agent's routine re-registration
/// costs nothing.
fn mesh_key_change(
    node: &temps_entities::nodes::Model,
    public_key: &str,
    endpoint: std::net::SocketAddr,
) -> Option<MeshKeyChange> {
    let old_public_key = node.mesh_wg_public_key.as_deref()?;
    (old_public_key != public_key).then(|| MeshKeyChange {
        node_id: node.id,
        node_name: node.name.clone(),
        old_public_key: old_public_key.to_string(),
        new_public_key: public_key.to_string(),
        old_endpoint: node.mesh_wg_endpoint.clone(),
        new_endpoint: endpoint.to_string(),
    })
}

/// Log and audit a node's mesh re-key. Every mesh member trusts the key as
/// this node, so a change is something an operator must be able to explain
/// later: a reinstalled agent, or something else holding the node's token.
async fn record_mesh_key_change(
    audit_service: &dyn temps_core::AuditLogger,
    change: MeshKeyChange,
    peer: std::net::SocketAddr,
) {
    warn!(
        node_id = change.node_id,
        node_name = %change.node_name,
        old_public_key = %change.old_public_key,
        new_public_key = %change.new_public_key,
        old_endpoint = change.old_endpoint.as_deref().unwrap_or("none"),
        new_endpoint = %change.new_endpoint,
        %peer,
        "node {} ({}) replaced its WireGuard mesh key",
        change.node_id,
        change.node_name
    );
    let audit = NodeMeshKeyChangedAudit {
        context: temps_core::AuditContext {
            // No user: a node authenticating with its own token. `0` is the
            // codebase's convention for an actor that isn't a user.
            user_id: 0,
            ip_address: Some(peer.ip().to_string()),
            user_agent: format!("temps-agent/node-{}", change.node_id),
        },
        node_id: change.node_id,
        node_name: change.node_name,
        old_public_key: change.old_public_key,
        new_public_key: change.new_public_key,
        old_endpoint: change.old_endpoint,
        new_endpoint: change.new_endpoint,
    };
    if let Err(error) = audit_service.create_audit_log(&audit).await {
        error!(
            node_id = audit.node_id,
            "node mesh key changed but the audit record failed: {error}"
        );
    }
}

/// The problem for a failed mesh registration: the node's own mistakes and
/// conflicts are its to fix, anything else is logged here.
fn register_problem(node_id: i32, error: MeshError) -> Problem {
    let status = match &error {
        MeshError::InvalidPublicKey | MeshError::InvalidEndpoint { .. } => StatusCode::BAD_REQUEST,
        MeshError::NodeNotFound(_) => StatusCode::NOT_FOUND,
        MeshError::Disabled
        | MeshError::PublicKeyInUse
        | MeshError::Exhausted { .. }
        | MeshError::PairingClosed
        | MeshError::TooManyPairings { .. }
        | MeshError::NotOnMesh(_) => StatusCode::CONFLICT,
        MeshError::Corrupt { .. }
        | MeshError::Database(_)
        | MeshError::InvalidCidr { .. }
        | MeshError::OverlapsComputePool { .. }
        | MeshError::InvalidPort(_)
        | MeshError::PortClashesWithVxlan(_)
        | MeshError::InUse { .. } => {
            error!(node_id, "WireGuard mesh registration failed: {error}");
            return problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("WireGuard Mesh Registration Failed")
                .with_detail(
                    "the control plane could not register this node on the WireGuard mesh; \
                     see its logs",
                );
        }
    };
    problemdetails::new(status)
        .with_title("WireGuard Mesh Registration Failed")
        .with_detail(error.to_string())
}

/// Allocate a mesh node's compute CIDR (idempotent). A node that joined with a
/// public address only gets a private underlay, and so an allocation, once
/// it registers on the mesh.
async fn allocate_after_mesh_registration(app_state: &NodeAppState, node_id: i32) {
    let allocator = PostgresAllocator::new(app_state.db.clone());
    match allocator.allocate_for_node(node_id).await {
        Ok(_) | Err(AllocatorError::AlreadyAllocated { .. }) => {}
        Err(error) => warn!(
            node_id,
            "compute_cidr allocation for a WireGuard mesh node failed; retried on its next poll: {error}"
        ),
    }
}

/// Resolve the node for `node_id` and check the caller's bearer token against
/// it (constant time).
async fn authenticate_node(
    app_state: &NodeAppState,
    headers: &HeaderMap,
    node_id: i32,
) -> Result<temps_entities::nodes::Model, Problem> {
    let token = extract_bearer_token(headers)?;
    let node = app_state
        .node_service
        .get_by_id(node_id)
        .await
        .map_err(Problem::from)?;
    let token_hash = sha256_hash(&token);
    if !constant_time_eq(node.token_hash.as_bytes(), token_hash.as_bytes()) {
        warn!(node_id, "Invalid network token");
        return Err(problemdetails::new(StatusCode::UNAUTHORIZED)
            .with_title("Invalid Token")
            .with_detail(format!("Invalid authentication token for node {}", node_id)));
    }
    Ok(node)
}

// ---------------------------------------------------------------------------
// Helpers (duplicated from handlers::nodes intentionally — moving them to
// a shared module would expand the blast radius of this PR; we'll dedupe
// in a follow-up).
// ---------------------------------------------------------------------------

fn extract_bearer_token(headers: &HeaderMap) -> Result<String, Problem> {
    let auth_header = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| {
            problemdetails::new(StatusCode::UNAUTHORIZED)
                .with_title("Missing Authorization")
                .with_detail("Bearer token required for node authentication")
        })?;
    let token = auth_header.strip_prefix("Bearer ").ok_or_else(|| {
        problemdetails::new(StatusCode::UNAUTHORIZED)
            .with_title("Invalid Authorization")
            .with_detail("Authorization header must use Bearer scheme")
    })?;
    Ok(token.to_string())
}

fn sha256_hash(input: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    hex::encode(hasher.finalize())
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut result = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        result |= x ^ y;
    }
    result == 0
}

// ---------------------------------------------------------------------------
// Tests for the wire-format `From` impls (pure logic, no DB).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ipnet::Ipv4Net;
    use std::net::{IpAddr, Ipv4Addr};
    use std::str::FromStr;
    use uuid::Uuid;

    #[test]
    fn peer_to_entry_serializes_strings() {
        let p = Peer {
            node_id: Uuid::from_u128(42),
            compute_cidr: Ipv4Net::from_str("172.20.5.0/24").unwrap(),
            underlay_address: IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5)),
        };
        let entry: PeerEntry = p.into();
        assert_eq!(entry.compute_cidr, "172.20.5.0/24");
        assert_eq!(entry.underlay_address, "10.0.0.5");
        assert!(entry
            .node_id
            .contains("00000000-0000-0000-0000-00000000002a"));
    }

    #[test]
    fn alloc_to_entry_serializes_strings() {
        let a = NodeAllocPersisted {
            node_id: 7,
            external_id: Uuid::from_u128(0xABCD),
            compute_cidr: Ipv4Net::from_str("172.20.7.0/24").unwrap(),
            bridge_address: IpAddr::V4(Ipv4Addr::new(172, 20, 7, 1)),
            underlay_address: IpAddr::V4(Ipv4Addr::new(10, 0, 0, 7)),
        };
        let entry: AllocEntry = a.into();
        assert_eq!(entry.compute_cidr, "172.20.7.0/24");
        assert_eq!(entry.bridge_address, "172.20.7.1");
        assert_eq!(entry.underlay_address, "10.0.0.7");
    }

    #[test]
    fn response_omits_alloc_when_none() {
        let resp = PeerListResponse {
            network: NetworkPoolEntry {
                compute_pool_cidr: "172.20.0.0/16".into(),
                subnet_prefix_len: 24,
            },
            alloc: None,
            peers: vec![],
            cluster_dns_enabled: false,
            wireguard: None,
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(!json.contains("alloc"), "alloc should be omitted: {}", json);
        assert!(json.contains("\"peers\":[]"));
    }

    #[test]
    fn response_includes_alloc_when_present() {
        let resp = PeerListResponse {
            network: NetworkPoolEntry {
                compute_pool_cidr: "172.20.0.0/16".into(),
                subnet_prefix_len: 24,
            },
            alloc: Some(AllocEntry {
                node_id: "abc".into(),
                compute_cidr: "172.20.1.0/24".into(),
                bridge_address: "172.20.1.1".into(),
                underlay_address: "10.0.0.1".into(),
            }),
            peers: vec![],
            cluster_dns_enabled: false,
            wireguard: None,
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"alloc\""));
        assert!(json.contains("172.20.1.0/24"));
    }

    #[test]
    fn response_always_serializes_cluster_dns_enabled() {
        // The field must always be present in the JSON (never skip_serializing_if)
        // so older workers that don't know about it default to `false` safely.
        let resp_disabled = PeerListResponse {
            network: NetworkPoolEntry {
                compute_pool_cidr: "172.20.0.0/16".into(),
                subnet_prefix_len: 24,
            },
            alloc: None,
            peers: vec![],
            cluster_dns_enabled: false,
            wireguard: None,
        };
        let json = serde_json::to_string(&resp_disabled).unwrap();
        assert!(
            json.contains("\"cluster_dns_enabled\":false"),
            "cluster_dns_enabled:false must be serialized, got: {}",
            json
        );

        let resp_enabled = PeerListResponse {
            network: NetworkPoolEntry {
                compute_pool_cidr: "172.20.0.0/16".into(),
                subnet_prefix_len: 24,
            },
            alloc: None,
            peers: vec![],
            cluster_dns_enabled: true,
            wireguard: None,
        };
        let json = serde_json::to_string(&resp_enabled).unwrap();
        assert!(
            json.contains("\"cluster_dns_enabled\":true"),
            "cluster_dns_enabled:true must be serialized, got: {}",
            json
        );
    }

    #[test]
    fn token_extraction_requires_bearer_prefix() {
        let mut h = HeaderMap::new();
        h.insert("authorization", "Basic xxx".parse().unwrap());
        let r = extract_bearer_token(&h);
        assert!(r.is_err());
    }

    #[test]
    fn token_extraction_strips_prefix() {
        let mut h = HeaderMap::new();
        h.insert("authorization", "Bearer secret-token".parse().unwrap());
        let r = extract_bearer_token(&h).unwrap();
        assert_eq!(r, "secret-token");
    }

    #[test]
    fn sha256_is_deterministic() {
        assert_eq!(sha256_hash("foo"), sha256_hash("foo"));
        assert_ne!(sha256_hash("foo"), sha256_hash("bar"));
    }

    #[test]
    fn constant_time_eq_handles_lengths() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"xyz"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
    }

    // ── Node-authenticated mesh endpoints ───────────────────────────────

    use crate::handlers::nodes::{NodeAppState, RegistrationRateLimiter};
    use crate::handlers::wireguard_mesh::admin_test_support::{
        config_service, encryption_service, mock_db, node,
    };
    use axum::body::Body;
    use axum::http::Request;
    use axum::routing::{get, put};
    use axum::Router;
    use sea_orm::DatabaseConnection;
    use tower::ServiceExt;

    const NODE_1_TOKEN: &str = "node-1-token";

    /// The agent-facing mesh routes over `db`.
    fn agent_app(db: DatabaseConnection) -> Router {
        let db = Arc::new(db);
        let state = Arc::new(NodeAppState {
            node_service: Arc::new(crate::services::NodeService::new(db.clone())),
            db: db.clone(),
            config_service: config_service(temps_core::AppSettings::default()),
            encryption_service: encryption_service(),
            telemetry: Arc::new(temps_core::telemetry::NoopTelemetryReporter),
            rate_limiter: Arc::new(RegistrationRateLimiter::new()),
            enrollment_token_service: Arc::new(temps_config::EnrollmentTokenService::new(db)),
            alarm_service: None,
            audit_service: Arc::new(
                crate::handlers::wireguard_mesh::admin_test_support::RecordingAuditLogger::default(
                ),
            ),
        });
        Router::new()
            .route("/internal/nodes/{node_id}/network/peers", get(list_peers))
            .route(
                "/internal/nodes/{node_id}/network/wireguard",
                put(register_mesh),
            )
            .route(
                "/internal/nodes/{node_id}/network/wireguard/handshakes",
                put(report_mesh_handshakes),
            )
            .with_state(state)
            .layer(axum::extract::connect_info::MockConnectInfo(
                std::net::SocketAddr::from(([198, 51, 100, 9], 40000)),
            ))
    }

    /// A database that knows node `id`, whose agent holds `token`.
    fn db_with_node(id: i32, token: &str) -> DatabaseConnection {
        mock_db()
            .append_query_results(vec![vec![node(id, &format!("worker-{id}"), token)]])
            .into_connection()
    }

    async fn call(
        app: Router,
        method: &str,
        uri: &str,
        token: Option<&str>,
        body: Option<serde_json::Value>,
    ) -> (StatusCode, serde_json::Value) {
        let mut request = Request::builder().method(method).uri(uri);
        if let Some(token) = token {
            request = request.header("authorization", format!("Bearer {token}"));
        }
        if body.is_some() {
            request = request.header("content-type", "application/json");
        }
        let request = request
            .body(body.map_or_else(Body::empty, |body| Body::from(body.to_string())))
            .unwrap();
        let response = app.oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, json)
    }

    fn mesh_endpoints(node_id: i32) -> Vec<(&'static str, String, Option<serde_json::Value>)> {
        vec![
            (
                "GET",
                format!("/internal/nodes/{node_id}/network/peers"),
                None,
            ),
            (
                "PUT",
                format!("/internal/nodes/{node_id}/network/wireguard"),
                Some(serde_json::json!({
                    "public_key": "bm9kZS1rZXktMzItYnl0ZXMtbG9uZy1wYWRkZWQhIQ==",
                    "endpoint": "203.0.113.10:51820"
                })),
            ),
            (
                "PUT",
                format!("/internal/nodes/{node_id}/network/wireguard/handshakes"),
                Some(serde_json::json!({"peers": []})),
            ),
        ]
    }

    #[tokio::test]
    async fn mesh_endpoints_refuse_a_missing_token() {
        for (method, uri, body) in mesh_endpoints(1) {
            let (status, problem) = call(
                agent_app(db_with_node(1, NODE_1_TOKEN)),
                method,
                &uri,
                None,
                body,
            )
            .await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {uri}");
            assert_eq!(problem["title"], "Missing Authorization", "{method} {uri}");
        }
    }

    #[tokio::test]
    async fn mesh_endpoints_refuse_a_wrong_token() {
        for (method, uri, body) in mesh_endpoints(1) {
            let (status, problem) = call(
                agent_app(db_with_node(1, NODE_1_TOKEN)),
                method,
                &uri,
                Some("not-the-token"),
                body,
            )
            .await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {uri}");
            assert_eq!(problem["title"], "Invalid Token", "{method} {uri}");
        }
    }

    #[tokio::test]
    async fn one_nodes_token_does_not_act_for_another_node() {
        // Node 1's agent calls node 2's paths: node 2 exists with its own
        // token, so node 1's is refused.
        for (method, uri, body) in mesh_endpoints(2) {
            let (status, _) = call(
                agent_app(db_with_node(2, "node-2-token")),
                method,
                &uri,
                Some(NODE_1_TOKEN),
                body,
            )
            .await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {uri}");
        }
        // And a node that does not exist is not found, whatever the token.
        let (status, _) = call(
            agent_app(
                mock_db()
                    .append_query_results(vec![Vec::<temps_entities::nodes::Model>::new()])
                    .into_connection(),
            ),
            "PUT",
            "/internal/nodes/9/network/wireguard/handshakes",
            Some(NODE_1_TOKEN),
            Some(serde_json::json!({"peers": []})),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn a_handshake_report_is_capped() {
        let peers: Vec<serde_json::Value> = (0..=MAX_REPORTED_PEERS)
            .map(|i| serde_json::json!({"public_key": format!("key-{i}"), "seconds_since_handshake": 5}))
            .collect();
        let (status, problem) = call(
            agent_app(db_with_node(1, NODE_1_TOKEN)),
            "PUT",
            "/internal/nodes/1/network/wireguard/handshakes",
            Some(NODE_1_TOKEN),
            Some(serde_json::json!({ "peers": peers })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(problem["title"], "Too Many Peers");
    }

    #[tokio::test]
    async fn a_registration_with_a_bad_endpoint_is_refused_after_authentication() {
        let (status, problem) = call(
            agent_app(db_with_node(1, NODE_1_TOKEN)),
            "PUT",
            "/internal/nodes/1/network/wireguard",
            Some(NODE_1_TOKEN),
            Some(serde_json::json!({"public_key": "k", "endpoint": "not-an-endpoint"})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(problem["title"], "Invalid WireGuard Endpoint");
    }

    const OLD_KEY: &str = "b2xkLWtleS0zMi1ieXRlcy1sb25nLXBhZGRlZCEhIQ==";
    const NEW_KEY: &str = "bmV3LWtleS0zMi1ieXRlcy1sb25nLXBhZGRlZCEhIQ==";

    fn on_mesh(key: Option<&str>) -> temps_entities::nodes::Model {
        let mut node = node(4, "worker-4", NODE_1_TOKEN);
        node.mesh_wg_public_key = key.map(str::to_string);
        node.mesh_wg_endpoint = key.map(|_| "203.0.113.4:51820".to_string());
        node
    }

    #[test]
    fn only_replacing_a_registered_key_is_a_key_change() {
        let endpoint: std::net::SocketAddr = "203.0.113.40:51820".parse().unwrap();
        // First registration: nothing to replace.
        assert_eq!(mesh_key_change(&on_mesh(None), NEW_KEY, endpoint), None);
        // The agent's routine re-registration, even from a new endpoint.
        assert_eq!(
            mesh_key_change(&on_mesh(Some(OLD_KEY)), OLD_KEY, endpoint),
            None
        );
        assert_eq!(
            mesh_key_change(&on_mesh(Some(OLD_KEY)), NEW_KEY, endpoint),
            Some(MeshKeyChange {
                node_id: 4,
                node_name: "worker-4".into(),
                old_public_key: OLD_KEY.into(),
                new_public_key: NEW_KEY.into(),
                old_endpoint: Some("203.0.113.4:51820".into()),
                new_endpoint: "203.0.113.40:51820".into(),
            })
        );
    }

    #[tokio::test]
    async fn a_key_change_is_audited_with_the_peer_that_made_it() {
        let audit =
            crate::handlers::wireguard_mesh::admin_test_support::RecordingAuditLogger::default();
        let change = mesh_key_change(
            &on_mesh(Some(OLD_KEY)),
            NEW_KEY,
            "203.0.113.40:51820".parse().unwrap(),
        )
        .unwrap();
        record_mesh_key_change(&audit, change, "198.51.100.9:40000".parse().unwrap()).await;

        let records = audit.records();
        assert_eq!(records.len(), 1, "{records:?}");
        assert_eq!(records[0].operation, "NODE_MESH_KEY_CHANGED");
        assert_eq!(records[0].ip_address.as_deref(), Some("198.51.100.9"));
        assert_eq!(records[0].user_agent, "temps-agent/node-4");
        let body: serde_json::Value = serde_json::from_str(&records[0].body).unwrap();
        assert_eq!(body["node_id"], 4);
        assert_eq!(body["old_public_key"], OLD_KEY);
        assert_eq!(body["new_public_key"], NEW_KEY);
        assert_eq!(body["old_endpoint"], "203.0.113.4:51820");
        assert_eq!(body["new_endpoint"], "203.0.113.40:51820");
    }

    #[test]
    fn registration_errors_map_to_the_nodes_fix_or_to_ours() {
        let pool: ipnet::Ipv4Net = "10.201.0.0/16".parse().unwrap();
        let cases = [
            (MeshError::InvalidPublicKey, StatusCode::BAD_REQUEST),
            (
                MeshError::InvalidEndpoint {
                    value: "x".into(),
                    reason: "y".into(),
                },
                StatusCode::BAD_REQUEST,
            ),
            (MeshError::NodeNotFound(1), StatusCode::NOT_FOUND),
            (MeshError::Disabled, StatusCode::CONFLICT),
            (MeshError::PublicKeyInUse, StatusCode::CONFLICT),
            (MeshError::Exhausted { cidr: pool }, StatusCode::CONFLICT),
            (MeshError::PairingClosed, StatusCode::CONFLICT),
            (
                MeshError::TooManyPairings { limit: 20 },
                StatusCode::CONFLICT,
            ),
            (
                MeshError::NotOnMesh("worker-1".into()),
                StatusCode::CONFLICT,
            ),
            (
                MeshError::Corrupt {
                    what: "network_config".into(),
                    reason: "missing".into(),
                },
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
            (
                MeshError::from(sea_orm::DbErr::Custom("reset".into())),
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
            (
                MeshError::InvalidCidr {
                    value: "x".into(),
                    reason: "y".into(),
                },
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
            (
                MeshError::OverlapsComputePool { mesh: pool, pool },
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
            (MeshError::InvalidPort(0), StatusCode::INTERNAL_SERVER_ERROR),
            (
                MeshError::PortClashesWithVxlan(4789),
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
            (
                MeshError::InUse {
                    setting: "pool",
                    current: pool.to_string(),
                    assigned: 1,
                },
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
        ];
        for (error, expected) in cases {
            let shown = error.to_string();
            let problem = register_problem(1, error);
            assert_eq!(problem.status_code, expected, "{shown}");
            let detail = problem.body.get("detail").and_then(|d| d.as_str());
            if expected == StatusCode::INTERNAL_SERVER_ERROR {
                assert_ne!(detail, Some(shown.as_str()), "internal detail leaked");
            } else {
                assert_eq!(detail, Some(shown.as_str()));
            }
        }
    }
}
