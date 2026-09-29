// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Admin view of the managed WireGuard mesh.
//!
//! `GET /nodes/wireguard` answers "can nodes that only share the internet
//! with this control plane join it, and are the ones that did connected?" —
//! including, when the mesh is off, what is missing and how to turn it on,
//! so the Worker Nodes page can onboard instead of hiding the option.
//! `POST /nodes/wireguard` turns the mesh on; the running `temps serve`
//! notices the setting change and brings its end up without a restart.

use std::sync::Arc;
use std::time::SystemTime;

use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use serde::{Deserialize, Serialize};
use temps_auth::{permission_guard, require_sensitive_action, RequireAuth};
use temps_core::node_address::is_private_node_address;
use temps_core::problemdetails::{self, Problem};
use temps_core::{AuditContext, SensitiveAction};
use temps_entities::nodes;
use temps_network::mesh::{MeshError, MeshPeerStatus, MeshSettings, LIVE_HANDSHAKE};
use tracing::error;
use utoipa::ToSchema;

use crate::handlers::audit::WireguardMeshEnabledAudit;
use crate::handlers::types::AppState;

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
}

/// Body of `POST /nodes/wireguard`. Both fields keep their current value
/// when omitted; neither can change once a node is on the mesh.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct EnableWireguardMeshRequest {
    /// Mesh address pool (private IPv4, clear of the compute pool).
    pub cidr: Option<String>,
    /// UDP port the mesh listens on.
    pub listen_port: Option<u16>,
    /// TCP port nodes reach the control plane's API on over the mesh.
    /// Defaults to the mesh port number.
    pub node_api_port: Option<u16>,
}

fn mesh_problem(error: MeshError) -> Problem {
    match &error {
        MeshError::InvalidCidr { .. }
        | MeshError::OverlapsComputePool { .. }
        | MeshError::InvalidPort(_)
        | MeshError::PortClashesWithVxlan(_) => problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Invalid WireGuard Mesh Settings")
            .with_detail(error.to_string()),
        MeshError::InUse { .. } => problemdetails::new(StatusCode::CONFLICT)
            .with_title("WireGuard Mesh Settings In Use")
            .with_detail(error.to_string()),
        _ => {
            error!("WireGuard mesh state failed: {error}");
            problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("WireGuard Mesh Error")
                .with_detail("could not read the WireGuard mesh state; see the server logs")
        }
    }
}

/// What prevents this server from bringing up the control plane's end of
/// the mesh, if anything.
fn enable_blocker() -> Option<String> {
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

async fn mesh_status(app_state: &AppState) -> Result<WireguardMeshStatusResponse, Problem> {
    let db = app_state.db.as_ref();
    let settings = temps_network::mesh::load_settings(db)
        .await
        .map_err(mesh_problem)?;
    let published = temps_network::mesh::published_control_plane(db)
        .await
        .map_err(mesh_problem)?;
    let listen_port = match &settings {
        Some(settings) => settings.port,
        None => temps_network::mesh::configured_port(db)
            .await
            .map_err(mesh_problem)?,
    };
    let (state, mut reason) = state_of(settings.as_ref(), published.is_some());
    if state == WireguardMeshState::Starting {
        if let Some(failure) = temps_network::control_plane::last_setup_failure() {
            reason = Some(format!(
                "The control plane could not bring up its end of the mesh: {failure}. It retries \
                 on its own once the cluster network settings change."
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
    let nodes = app_state
        .node_service
        .list_all()
        .await
        .map_err(Problem::from)?
        .into_iter()
        .map(|node| {
            let peer = peers.as_ref().and_then(|peers| {
                node.mesh_wg_public_key
                    .as_deref()
                    .and_then(|key| peers.iter().find(|peer| peer.public_key == key))
            });
            let connection = node_connection(state, &node, peer, peers.is_some(), now);
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
            }
        })
        .collect();

    let blocker = if settings.is_none() {
        enable_blocker()
    } else {
        None
    };
    let join_url = app_state
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
    })
}

/// Mesh state, per-node connection and join onboarding for the Worker Nodes
/// page.
#[utoipa::path(
    tag = "Nodes",
    get,
    path = "/nodes/wireguard",
    operation_id = "WireguardMeshStatusGet",
    responses(
        (status = 200, description = "WireGuard mesh state", body = WireguardMeshStatusResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn wireguard_mesh_status(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsRead);
    Ok(Json(mesh_status(&app_state).await?))
}

/// Turn on the cluster's WireGuard mesh (idempotent). The running control
/// plane brings its end up within a minute and nodes follow; it cannot be
/// turned off again from the API.
#[utoipa::path(
    tag = "Nodes",
    post,
    path = "/nodes/wireguard",
    operation_id = "WireguardMeshEnable",
    request_body = EnableWireguardMeshRequest,
    responses(
        (status = 200, description = "Mesh enabled; current state", body = WireguardMeshStatusResponse),
        (status = 400, description = "Invalid pool or port"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 409, description = "This server cannot bring up the mesh, or the pool/port is in use"),
        (status = 428, description = "Re-authentication required"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn enable_wireguard_mesh(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Json(request): Json<EnableWireguardMeshRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);
    require_sensitive_action(
        app_state.sensitive_action_authorizer.as_ref(),
        &auth,
        SensitiveAction::EnableWireguardMesh,
    )
    .await?;

    let already_on = temps_network::mesh::load_settings(app_state.db.as_ref())
        .await
        .map_err(mesh_problem)?
        .is_some();
    if !already_on {
        if let Some(blocker) = enable_blocker() {
            return Err(problemdetails::new(StatusCode::CONFLICT)
                .with_title("WireGuard Mesh Unavailable Here")
                .with_detail(blocker));
        }
    }

    let settings = temps_network::mesh::enable(
        app_state.db.as_ref(),
        request.cidr.as_deref().map(str::trim),
        request.listen_port,
        request.node_api_port,
    )
    .await
    .map_err(mesh_problem)?;

    let audit = WireguardMeshEnabledAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: None,
            user_agent: "temps-api".to_string(),
        },
        cidr: settings.cidr.to_string(),
        listen_port: settings.port,
    };
    if let Err(error) = app_state.audit_service.create_audit_log(&audit).await {
        error!(%error, "WireGuard mesh enabled but audit record failed");
    }

    Ok(Json(mesh_status(&app_state).await?))
}

#[cfg(test)]
mod tests {
    use super::*;
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
}
