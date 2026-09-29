// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! One-paste node pairing (ADR 048 D2b).
//!
//! For a node the control plane can reach when nodes cannot reach the
//! control plane (a control plane on a laptop or behind NAT): the operator
//! enters the node's address, runs the returned `temps join --pair <code>` on
//! it, and the control plane dials the node to learn its WireGuard public
//! key. Private keys never leave their host.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use temps_auth::{permission_guard, require_sensitive_action, RequireAuth};
use temps_core::problemdetails::{self, Problem};
use temps_core::{AuditContext, SensitiveAction};
use temps_entities::node_pairings;
use temps_network::mesh::MeshError;
use temps_wireguard::pairing::{PairingCode, PairingId, PairingSecret};
use tracing::error;
use utoipa::ToSchema;

use crate::handlers::audit::{NodePairingCancelledAudit, NodePairingCreatedAudit};
use crate::handlers::types::AppState;

/// How long a pairing code stays usable.
const PAIRING_TTL_SECS: i64 = 30 * 60;

/// Start pairing a node.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct CreateNodePairingRequest {
    /// The node's public address, `ip` or `ip:port`. The control plane dials
    /// it on the mesh UDP port (or the port given).
    pub address: String,
    /// Name the node registers under. Defaults to `worker-<random>`.
    pub name: Option<String>,
}

/// A pairing as the Worker Nodes page shows it.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct NodePairingResponse {
    pub id: i32,
    pub name: String,
    /// `ip:port` the control plane dials.
    pub node_endpoint: String,
    /// Mesh address reserved for the node.
    pub mesh_address: String,
    /// `waiting` (dialing the node), `key_received` (the node answered; it is
    /// registering over the mesh), `completed`, `expired` or `cancelled`.
    pub status: String,
    /// Why the last attempt to reach the node failed.
    pub last_error: Option<String>,
    /// Why the control plane last refused the node's key (e.g. it belongs to
    /// another node). Kept until a key is accepted, so it outlives the
    /// "no answer" attempts after the refused node stopped.
    pub last_rejection: Option<String>,
    pub last_attempt_at: Option<String>,
    pub expires_at: String,
    /// The node that registered with this pairing.
    pub node_id: Option<i32>,
    pub created_at: String,
}

impl From<node_pairings::Model> for NodePairingResponse {
    fn from(model: node_pairings::Model) -> Self {
        Self {
            id: model.id,
            name: model.name,
            node_endpoint: model.node_endpoint,
            mesh_address: model.mesh_address,
            status: model.status,
            last_error: model.last_error,
            last_rejection: model.last_rejection,
            last_attempt_at: model.last_attempt_at.map(|at| at.to_rfc3339()),
            expires_at: model.expires_at.to_rfc3339(),
            node_id: model.node_id,
            created_at: model.created_at.to_rfc3339(),
        }
    }
}

/// A new pairing and the one command to run on the node. The command holds
/// a secret and is returned only here.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CreateNodePairingResponse {
    pub pairing: NodePairingResponse,
    /// Run this on the node, as root: `temps join --pair <code>`.
    pub join_command: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct NodePairingListResponse {
    pub pairings: Vec<NodePairingResponse>,
}

fn problem(error: MeshError) -> Problem {
    match error {
        MeshError::Disabled => problemdetails::new(StatusCode::CONFLICT)
            .with_title("WireGuard Mesh Off")
            .with_detail("Turn the WireGuard mesh on (Worker Nodes → Over the internet) before pairing a node."),
        MeshError::TooManyPairings { limit } => problemdetails::new(StatusCode::CONFLICT)
            .with_title("Too Many Pairings In Progress")
            .with_detail(format!(
                "{limit} pairings are already waiting for their nodes. Cancel the ones you no \
                 longer need (Worker Nodes, or `bunx @temps-sdk/cli nodes pair`), or let them \
                 expire after 30 minutes."
            )),
        MeshError::InvalidEndpoint { .. } | MeshError::Exhausted { .. } => {
            problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Cannot Pair This Node")
                .with_detail(error.to_string())
        }
        _ => {
            error!("node pairing failed: {error}");
            problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Node Pairing Error")
                .with_detail("could not update the node pairings; see the server logs")
        }
    }
}

fn internal(what: &str) -> impl FnOnce(String) -> Problem + '_ {
    move |reason| {
        error!("node pairing: {what}: {reason}");
        problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
            .with_title("Node Pairing Error")
            .with_detail(format!("could not {what}; see the server logs"))
    }
}

/// The node's WireGuard endpoint from what the operator typed.
///
/// Private (RFC 1918) addresses are accepted on purpose: self-hosted nodes
/// on a LAN or VPC are paired by their private address. What the control
/// plane sends there is a fixed-size, MAC-authenticated UDP datagram, and
/// nothing comes back without the pairing secret, so a `SettingsWrite`
/// operator aiming it at an internal host learns nothing; loopback,
/// link-local and cloud metadata addresses are refused by `parse_endpoint`.
fn parse_node_endpoint(value: &str, mesh_port: u16) -> Result<SocketAddr, Problem> {
    let value = value.trim();
    let endpoint = value
        .parse::<SocketAddr>()
        .or_else(|_| {
            value
                .parse::<IpAddr>()
                .map(|ip| SocketAddr::new(ip, mesh_port))
        })
        .map_err(|_| {
            problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Invalid Node Address")
                .with_detail(format!(
                    "'{value}' is not an IP address or ip:port (hostnames are not accepted)"
                ))
        })?;
    temps_network::mesh::parse_endpoint(&endpoint.to_string()).map_err(|error| {
        problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Invalid Node Address")
            .with_detail(error.to_string())
    })
}

/// A node name: lowercase letters, digits and dashes, 1–63 characters.
fn valid_name(name: &str) -> bool {
    (1..=63).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !name.starts_with('-')
}

/// Pair a node the control plane can reach: returns the command to run on
/// it. The control plane then dials the node until it answers or the pairing
/// expires (30 minutes).
#[utoipa::path(
    tag = "Nodes",
    post,
    path = "/nodes/pairings",
    operation_id = "NodePairingCreate",
    request_body = CreateNodePairingRequest,
    responses(
        (status = 201, description = "Pairing created", body = CreateNodePairingResponse),
        (status = 400, description = "Invalid address or name"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 409, description = "The mesh is off or its control-plane end is not up"),
        (status = 428, description = "Re-authentication required"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn create_node_pairing(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Json(request): Json<CreateNodePairingRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);
    require_sensitive_action(
        app_state.sensitive_action_authorizer.as_ref(),
        &auth,
        SensitiveAction::CreateNodePairing,
    )
    .await?;
    let db = app_state.db.as_ref();

    let settings = temps_network::mesh::load_settings(db)
        .await
        .map_err(problem)?
        .ok_or_else(|| problem(MeshError::Disabled))?;
    let control_plane = temps_network::mesh::published_control_plane(db)
        .await
        .map_err(problem)?
        .ok_or_else(|| {
            problemdetails::new(StatusCode::CONFLICT)
                .with_title("Mesh Not Ready")
                .with_detail(
                    "The control plane has not brought up its end of the mesh yet. Wait for the \
                     mesh to show Ready, then pair the node.",
                )
        })?;
    let node_endpoint = parse_node_endpoint(&request.address, settings.port)?;

    let pairing_id =
        PairingId::generate().map_err(|e| internal("generate a pairing")(e.to_string()))?;
    let secret =
        PairingSecret::generate().map_err(|e| internal("generate a pairing")(e.to_string()))?;
    let name = match request
        .name
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
    {
        Some(name) if valid_name(name) => name.to_string(),
        Some(_) => {
            return Err(problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Invalid Node Name")
                .with_detail("Use 1–63 lowercase letters, digits and dashes."))
        }
        None => format!(
            "worker-{}",
            &pairing_id
                .to_base64url()
                .to_ascii_lowercase()
                .replace(['-', '_'], "")[..6]
        ),
    };

    let ca = crate::cluster_ca::ensure_cluster_ca(
        app_state.config_service.as_ref(),
        app_state.encryption_service.as_ref(),
    )
    .await
    .map_err(|e| internal("initialize the cluster CA")(e.to_string()))?;
    let ca_fingerprint = temps_core::node_pki::ca_fingerprint_sha256(&ca.cert_pem)
        .map_err(|e| internal("fingerprint the cluster CA")(e.to_string()))?;

    let (join_token, token) = temps_config::EnrollmentTokenService::new(app_state.db.clone())
        .mint(temps_config::enrollment_tokens::MintParams {
            max_uses: 1,
            ttl_secs: PAIRING_TTL_SECS,
            bound_node_name: Some(name.clone()),
            bound_labels: None,
            created_by_user_id: Some(auth.user_id()),
            ca_fingerprint: Some(ca_fingerprint.clone()),
        })
        .await
        .map_err(|e| internal("mint the enrollment token")(e.to_string()))?;
    let secret_encrypted = app_state
        .encryption_service
        .encrypt(secret.to_base64url().as_bytes())
        .map_err(|e| internal("encrypt the pairing secret")(e.to_string()))?;

    let created = temps_network::pairing::create(
        db,
        temps_network::pairing::NewPairing {
            pairing_id: pairing_id.to_base64url(),
            name: name.clone(),
            node_endpoint,
            secret_encrypted,
            enrollment_token_id: token.id,
            expires_at: token.expires_at,
            created_by_user_id: Some(auth.user_id()),
        },
    )
    .await;
    let pairing = match created {
        Ok(pairing) => pairing,
        Err(error) => {
            // The token was never handed out; do not leave it usable.
            if let Err(revoke_error) =
                temps_config::EnrollmentTokenService::new(app_state.db.clone())
                    .revoke(token.id)
                    .await
            {
                error!(%revoke_error, "could not revoke the enrollment token of a pairing that was not created");
            }
            return Err(problem(error));
        }
    };

    let node_address = pairing
        .mesh_address
        .parse()
        .map_err(|e: std::net::AddrParseError| internal("reserve a mesh address")(e.to_string()))?;
    let code = PairingCode {
        id: pairing_id.to_base64url(),
        secret: secret.to_base64url(),
        name: pairing.name.clone(),
        control_plane_public_key: control_plane.public_key,
        control_plane_endpoint: control_plane.endpoint,
        control_plane_address: settings.control_plane_address(),
        node_address,
        node_endpoint,
        prefix_len: settings.cidr.prefix_len(),
        listen_port: settings.port,
        node_api_port: settings.node_api_port,
        ca_fingerprint,
        join_token,
        expires_at: pairing.expires_at.timestamp(),
    };

    let audit = NodePairingCreatedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: None,
            user_agent: "temps-api".to_string(),
        },
        pairing_id: pairing.id,
        name: pairing.name.clone(),
        node_endpoint: pairing.node_endpoint.clone(),
    };
    if let Err(error) = app_state.audit_service.create_audit_log(&audit).await {
        error!(%error, "node pairing created but audit record failed");
    }

    Ok((
        StatusCode::CREATED,
        Json(CreateNodePairingResponse {
            join_command: format!("temps join --pair {}", code.encode()),
            pairing: pairing.into(),
        }),
    ))
}

/// Recent node pairings, newest first.
#[utoipa::path(
    tag = "Nodes",
    get,
    path = "/nodes/pairings",
    operation_id = "NodePairingList",
    responses(
        (status = 200, description = "Node pairings", body = NodePairingListResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_node_pairings(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsRead);
    let pairings = temps_network::pairing::list(app_state.db.as_ref())
        .await
        .map_err(problem)?;
    Ok(Json(NodePairingListResponse {
        pairings: pairings.into_iter().map(Into::into).collect(),
    }))
}

/// Cancel a pending pairing: the control plane stops dialing the node, the
/// code stops working and its mesh address is released.
#[utoipa::path(
    tag = "Nodes",
    delete,
    path = "/nodes/pairings/{pairing_id}",
    operation_id = "NodePairingCancel",
    params(("pairing_id" = i32, Path, description = "Pairing ID")),
    responses(
        (status = 204, description = "Pairing cancelled"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "No such pairing"),
        (status = 409, description = "The pairing already finished"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn cancel_node_pairing(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Path(pairing_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);
    let db = app_state.db.as_ref();
    let pairing = temps_network::pairing::get(db, pairing_id)
        .await
        .map_err(problem)?
        .ok_or_else(|| {
            problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Pairing Not Found")
                .with_detail(format!("No node pairing with id {pairing_id}"))
        })?;
    if !temps_network::pairing::cancel(db, pairing_id)
        .await
        .map_err(problem)?
    {
        return Err(problemdetails::new(StatusCode::CONFLICT)
            .with_title("Pairing Already Finished")
            .with_detail(format!(
                "This pairing is {}; nothing to cancel.",
                pairing.status
            )));
    }
    if let Err(error) = temps_config::EnrollmentTokenService::new(app_state.db.clone())
        .revoke(pairing.enrollment_token_id)
        .await
    {
        error!(%error, pairing = pairing_id, "pairing cancelled but its enrollment token could not be revoked");
    }
    let audit = NodePairingCancelledAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: None,
            user_agent: "temps-api".to_string(),
        },
        pairing_id,
        name: pairing.name,
    };
    if let Err(error) = app_state.audit_service.create_audit_log(&audit).await {
        error!(%error, "node pairing cancelled but audit record failed");
    }
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_addresses_take_the_mesh_port_unless_given() {
        assert_eq!(
            parse_node_endpoint("198.51.100.7", 51820).unwrap(),
            "198.51.100.7:51820".parse().unwrap()
        );
        assert_eq!(
            parse_node_endpoint(" 198.51.100.7:4000 ", 51820).unwrap(),
            "198.51.100.7:4000".parse().unwrap()
        );
        assert!(parse_node_endpoint("node.example.com", 51820).is_err());
        assert!(parse_node_endpoint("169.254.169.254", 51820).is_err());
    }

    #[test]
    fn node_names_are_dns_labels() {
        assert!(valid_name("worker-1"));
        assert!(!valid_name("Worker"));
        assert!(!valid_name("-worker"));
        assert!(!valid_name(&"a".repeat(64)));
    }
}
