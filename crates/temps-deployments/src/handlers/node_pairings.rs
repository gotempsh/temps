// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! One-paste node pairing (ADR 048 D2b).
//!
//! For a node the control plane can reach when nodes cannot reach the
//! control plane (a control plane on a laptop or behind NAT): the operator
//! enters the node's address, runs the returned `temps join --pair <code>` on
//! it, and the control plane dials the node to learn its WireGuard public
//! key. Private keys never leave their host.
//!
//! The logic lives in [`NodePairingAdminService`]; these handlers check
//! permissions, parse the node's address, record the audit trail and map
//! errors to problems.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use axum::{
    extract::Path,
    http::{header, StatusCode},
    response::IntoResponse,
    Extension, Json,
};
use serde::{Deserialize, Serialize};
use temps_auth::{permission_guard, require_sensitive_action, RequireAuth};
use temps_core::problemdetails::{self, Problem};
use temps_core::{
    AuditContext, AuditLogger, RequestMetadata, SensitiveAction, SensitiveActionAuthorizer,
};
use temps_entities::node_pairings;
use temps_network::mesh::MeshError;
use tracing::error;
use utoipa::ToSchema;

use crate::handlers::audit::{NodePairingCancelledAudit, NodePairingCreatedAudit};
use crate::services::node_pairing_admin::{
    NodePairingAdminError, NodePairingAdminService, StartedPairing,
};

/// What the pairing admin handlers need. Provided to them as a request
/// extension by the deployments plugin.
#[derive(Clone)]
pub struct NodePairingAdminState {
    pub pairing_service: Arc<NodePairingAdminService>,
    pub audit_service: Arc<dyn AuditLogger>,
    pub sensitive_action_authorizer: Arc<dyn SensitiveActionAuthorizer>,
}

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

/// The problem for a mesh error met while pairing a node.
pub(crate) fn problem(error: MeshError) -> Problem {
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
        MeshError::PairingClosed => problemdetails::new(StatusCode::CONFLICT)
            .with_title("Pairing Already Finished")
            .with_detail(error.to_string()),
        MeshError::NodeNotFound(_) => problemdetails::new(StatusCode::NOT_FOUND)
            .with_title("Node Not Found")
            .with_detail(error.to_string()),
        // Stored settings that no longer parse (the settings errors come
        // from reading them, never from a pairing request), storage errors,
        // and errors of node registration and the hub, which pairing never
        // returns.
        MeshError::Corrupt { .. }
        | MeshError::Database(_)
        | MeshError::InvalidCidr { .. }
        | MeshError::OverlapsComputePool { .. }
        | MeshError::InvalidPort(_)
        | MeshError::PortClashesWithVxlan(_)
        | MeshError::InUse { .. }
        | MeshError::InvalidPublicKey
        | MeshError::PublicKeyInUse
        | MeshError::NotOnMesh(_) => {
            error!("node pairing failed: {error}");
            problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Node Pairing Error")
                .with_detail("could not update the node pairings; see the server logs")
        }
    }
}

fn internal(what: &str, error: &NodePairingAdminError) -> Problem {
    error!("node pairing: {error}");
    problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
        .with_title("Node Pairing Error")
        .with_detail(format!("could not {what}; see the server logs"))
}

/// The problem for a failed pairing operation.
pub(crate) fn pairing_problem(error: NodePairingAdminError) -> Problem {
    match error {
        NodePairingAdminError::Mesh { source, .. } => problem(source),
        NodePairingAdminError::MeshNotReady => problemdetails::new(StatusCode::CONFLICT)
            .with_title("Mesh Not Ready")
            .with_detail(
                "The control plane has not brought up its end of the mesh yet. Wait for the \
                 mesh to show Ready, then pair the node.",
            ),
        NodePairingAdminError::InvalidName { .. } => problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Invalid Node Name")
            .with_detail("Use 1–63 lowercase letters, digits and dashes."),
        NodePairingAdminError::NotFound { pairing_id } => {
            problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Pairing Not Found")
                .with_detail(format!("No node pairing with id {pairing_id}"))
        }
        NodePairingAdminError::AlreadyFinished { ref status, .. } => {
            problemdetails::new(StatusCode::CONFLICT)
                .with_title("Pairing Already Finished")
                .with_detail(format!("This pairing is {status}; nothing to cancel."))
        }
        NodePairingAdminError::Randomness { .. } => internal("generate a pairing", &error),
        NodePairingAdminError::ClusterCa(_) => internal("initialize the cluster CA", &error),
        NodePairingAdminError::CaFingerprint(_) => internal("fingerprint the cluster CA", &error),
        NodePairingAdminError::MintToken { .. } => internal("mint the enrollment token", &error),
        NodePairingAdminError::EncryptSecret { .. } => {
            internal("encrypt the pairing secret", &error)
        }
        NodePairingAdminError::InvalidMeshAddress { .. } => {
            internal("reserve a mesh address", &error)
        }
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
pub(crate) fn parse_node_endpoint(value: &str, mesh_port: u16) -> Result<SocketAddr, Problem> {
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
    Extension(state): Extension<NodePairingAdminState>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<CreateNodePairingRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);
    require_sensitive_action(
        state.sensitive_action_authorizer.as_ref(),
        &auth,
        SensitiveAction::CreateNodePairing,
    )
    .await?;
    let mesh_port = state
        .pairing_service
        .mesh_port()
        .await
        .map_err(pairing_problem)?;
    let node_endpoint = parse_node_endpoint(&request.address, mesh_port)?;
    let StartedPairing { pairing, code } = start_pairing_audited(
        &state.pairing_service,
        state.audit_service.as_ref(),
        AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        node_endpoint,
        request.name.as_deref(),
    )
    .await?;
    // The join command carries the pairing secret: never cache it.
    Ok((
        StatusCode::CREATED,
        [(header::CACHE_CONTROL, "no-store")],
        Json(CreateNodePairingResponse {
            join_command: format!("temps join --pair {}", code.as_str()),
            pairing: pairing.into(),
        }),
    ))
}

/// Create a pairing for a node at `node_endpoint` and record it in the audit
/// log as done by `context`. The returned code holds the secret: hand it
/// only to the node.
pub(crate) async fn start_pairing_audited(
    pairing_service: &NodePairingAdminService,
    audit_service: &dyn AuditLogger,
    context: AuditContext,
    node_endpoint: SocketAddr,
    name: Option<&str>,
) -> Result<StartedPairing, Problem> {
    let started = pairing_service
        .start(context.user_id, node_endpoint, name)
        .await
        .map_err(pairing_problem)?;
    let audit = NodePairingCreatedAudit {
        context,
        pairing_id: started.pairing.id,
        name: started.pairing.name.clone(),
        node_endpoint: started.pairing.node_endpoint.clone(),
    };
    if let Err(error) = audit_service.create_audit_log(&audit).await {
        error!(%error, "node pairing created but audit record failed");
    }
    Ok(started)
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
    Extension(state): Extension<NodePairingAdminState>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsRead);
    let pairings = state
        .pairing_service
        .list()
        .await
        .map_err(pairing_problem)?;
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
    Extension(state): Extension<NodePairingAdminState>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(pairing_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);
    let pairing = state
        .pairing_service
        .cancel(pairing_id)
        .await
        .map_err(pairing_problem)?;
    let audit = NodePairingCancelledAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        pairing_id,
        name: pairing.name,
    };
    if let Err(error) = state.audit_service.create_audit_log(&audit).await {
        error!(%error, "node pairing cancelled but audit record failed");
    }
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handlers::wireguard_mesh::admin_test_support::*;
    use axum::routing::{delete, get};
    use axum::Router;
    use sea_orm::{DatabaseConnection, MockExecResult};
    use std::collections::BTreeMap;

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
    fn pairing_errors_map_to_the_callers_fix_or_to_ours() {
        let status = |error| pairing_problem(error).status_code;
        assert_eq!(
            status(NodePairingAdminError::Mesh {
                action: "pair a node",
                source: MeshError::Disabled,
            }),
            StatusCode::CONFLICT
        );
        assert_eq!(
            status(NodePairingAdminError::Mesh {
                action: "create the pairing",
                source: MeshError::TooManyPairings { limit: 20 },
            }),
            StatusCode::CONFLICT
        );
        assert_eq!(
            status(NodePairingAdminError::Mesh {
                action: "create the pairing",
                source: MeshError::InvalidEndpoint {
                    value: "10.201.0.9:51820".into(),
                    reason: "inside the mesh pool".into(),
                },
            }),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            status(NodePairingAdminError::MeshNotReady),
            StatusCode::CONFLICT
        );
        assert_eq!(
            status(NodePairingAdminError::InvalidName {
                name: "Worker".into()
            }),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            status(NodePairingAdminError::NotFound { pairing_id: 3 }),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            status(NodePairingAdminError::AlreadyFinished {
                pairing_id: 3,
                status: "completed".into()
            }),
            StatusCode::CONFLICT
        );
        assert_eq!(
            status(NodePairingAdminError::Mesh {
                action: "list the node pairings",
                source: MeshError::from(sea_orm::DbErr::Custom("connection reset".into())),
            }),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(
            status(NodePairingAdminError::EncryptSecret {
                name: "worker-1".into(),
                reason: "bad key".into()
            }),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    fn pairing(id: i32, status: &str) -> node_pairings::Model {
        node_pairings::Model {
            id,
            pairing_id: "cGFpcmluZy1pZC0xNmJ5dGU".to_string(),
            name: "worker-7".to_string(),
            node_endpoint: "198.51.100.7:51820".to_string(),
            mesh_address: "10.201.0.2".to_string(),
            secret_encrypted: "encrypted".to_string(),
            enrollment_token_id: 11,
            public_key: None,
            status: status.to_string(),
            last_error: None,
            last_rejection: None,
            last_attempt_at: None,
            key_received_at: None,
            expires_at: chrono::Utc::now() + chrono::Duration::minutes(30),
            node_id: None,
            dialing_until: None,
            created_by_user_id: Some(1),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    fn enrollment_token(id: i32) -> temps_entities::node_enrollment_tokens::Model {
        temps_entities::node_enrollment_tokens::Model {
            id,
            token_hash: "0".repeat(64),
            max_uses: 1,
            used_count: 0,
            expires_at: chrono::Utc::now() + chrono::Duration::minutes(30),
            bound_node_name: Some("worker-7".to_string()),
            bound_labels: None,
            created_by_user_id: Some(1),
            revoked_at: None,
            ca_fingerprint: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    /// Settings holding a cluster CA, so pairing does not have to create one.
    fn settings_with_cluster_ca() -> temps_core::AppSettings {
        let ca = temps_core::node_pki::generate_cluster_ca().expect("cluster CA");
        let mut settings = temps_core::AppSettings::default();
        settings.multi_node.cluster_ca_key_encrypted = Some(
            encryption_service()
                .encrypt(ca.key_pem.as_bytes())
                .expect("encrypt CA key"),
        );
        settings.multi_node.cluster_ca_cert_pem = Some(ca.cert_pem);
        settings
    }

    fn app(
        db: DatabaseConnection,
        tokens_db: DatabaseConnection,
        settings: temps_core::AppSettings,
        authorizer: Arc<dyn SensitiveActionAuthorizer>,
        audit: Arc<RecordingAuditLogger>,
    ) -> Router {
        let state = NodePairingAdminState {
            pairing_service: Arc::new(NodePairingAdminService::new(
                Arc::new(db),
                config_service(settings),
                encryption_service(),
                Arc::new(temps_config::EnrollmentTokenService::new(Arc::new(
                    tokens_db,
                ))),
            )),
            audit_service: audit,
            sensitive_action_authorizer: authorizer,
        };
        Router::new()
            .route(
                "/nodes/pairings",
                get(list_node_pairings).post(create_node_pairing),
            )
            .route("/nodes/pairings/{pairing_id}", delete(cancel_node_pairing))
            .layer(Extension(state))
    }

    fn simple_app(
        db: DatabaseConnection,
        authorizer: Arc<dyn SensitiveActionAuthorizer>,
        audit: Arc<RecordingAuditLogger>,
    ) -> Router {
        app(
            db,
            mock_db().into_connection(),
            temps_core::AppSettings::default(),
            authorizer,
            audit,
        )
    }

    fn quiet_app() -> Router {
        simple_app(
            mock_db().into_connection(),
            Arc::new(AllowSensitiveActions),
            Arc::new(RecordingAuditLogger::default()),
        )
    }

    fn create_body() -> serde_json::Value {
        serde_json::json!({"address": "198.51.100.7", "name": "worker-7"})
    }

    // ── authentication and permissions ──────────────────────────────────

    #[tokio::test]
    async fn every_pairing_endpoint_requires_authentication() {
        for (method, uri, body) in [
            ("GET", "/nodes/pairings", None),
            ("POST", "/nodes/pairings", Some(create_body())),
            ("DELETE", "/nodes/pairings/3", None),
        ] {
            let (status, _, _) = send(quiet_app(), method, uri, body, None).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {uri}");
        }
    }

    #[tokio::test]
    async fn listing_pairings_needs_settings_read() {
        let (status, _, _) = send(
            quiet_app(),
            "GET",
            "/nodes/pairings",
            None,
            Some(outsider()),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn creating_or_cancelling_a_pairing_needs_settings_write() {
        for (method, uri, body) in [
            ("POST", "/nodes/pairings", Some(create_body())),
            ("DELETE", "/nodes/pairings/3", None),
        ] {
            let (status, _, _) =
                send(quiet_app(), method, uri, body, Some(settings_reader())).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}");
        }
    }

    #[tokio::test]
    async fn creating_a_pairing_asks_for_step_up() {
        let audit = Arc::new(RecordingAuditLogger::default());
        let app = simple_app(
            mock_db().into_connection(),
            Arc::new(RequireStepUp),
            audit.clone(),
        );
        let (status, _, body) = send(
            app,
            "POST",
            "/nodes/pairings",
            Some(create_body()),
            Some(admin()),
        )
        .await;
        assert_eq!(status, StatusCode::PRECONDITION_REQUIRED);
        assert_eq!(body["error_code"], "STEP_UP_REQUIRED");
        assert!(audit.records().is_empty());
    }

    // ── success and domain errors ───────────────────────────────────────

    #[tokio::test]
    async fn pairings_are_listed() {
        let db = mock_db()
            .append_query_results(vec![vec![pairing(3, "waiting")]])
            .into_connection();
        let app = simple_app(
            db,
            Arc::new(AllowSensitiveActions),
            Arc::new(RecordingAuditLogger::default()),
        );
        let (status, _, body) =
            send(app, "GET", "/nodes/pairings", None, Some(settings_reader())).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["pairings"][0]["id"], 3);
        assert_eq!(body["pairings"][0]["status"], "waiting");
        assert_eq!(body["pairings"][0]["node_endpoint"], "198.51.100.7:51820");
        assert!(
            body["pairings"][0].get("secret_encrypted").is_none(),
            "{body}"
        );
    }

    #[tokio::test]
    async fn a_new_pairing_returns_an_uncacheable_join_command_and_is_audited() {
        let count = |n: i64| {
            let mut row = BTreeMap::new();
            row.insert("num_items".to_string(), sea_orm::Value::BigInt(Some(n)));
            row
        };
        let db = mock_db()
            // mesh_port, then start: load_settings, published_control_plane
            .append_query_results(vec![vec![network_config(true, true)]])
            .append_query_results(vec![vec![network_config(true, true)]])
            .append_query_results(vec![vec![network_config(true, true)]])
            // create: the locked config row, pending pairings, taken
            // addresses (nodes, then pairings), the insert
            .append_query_results(vec![vec![network_config(true, true)]])
            .append_query_results(vec![vec![count(0)]])
            .append_query_results(vec![Vec::<temps_entities::nodes::Model>::new()])
            .append_query_results(vec![Vec::<node_pairings::Model>::new()])
            .append_query_results(vec![vec![pairing(5, "waiting")]])
            .into_connection();
        let tokens_db = mock_db()
            .append_query_results(vec![vec![enrollment_token(11)]])
            .into_connection();
        let audit = Arc::new(RecordingAuditLogger::default());
        let app = app(
            db,
            tokens_db,
            settings_with_cluster_ca(),
            Arc::new(AllowSensitiveActions),
            audit.clone(),
        );
        let (status, headers, body) = send(
            app,
            "POST",
            "/nodes/pairings",
            Some(create_body()),
            Some(admin()),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert_eq!(
            headers
                .get(header::CACHE_CONTROL)
                .and_then(|v| v.to_str().ok()),
            Some("no-store")
        );
        assert!(
            body["join_command"]
                .as_str()
                .is_some_and(|command| command.starts_with("temps join --pair ")),
            "{body}"
        );
        assert_eq!(body["pairing"]["id"], 5);

        let records = audit.records();
        assert_eq!(records.len(), 1, "{records:?}");
        assert_eq!(records[0].operation, "NODE_PAIRING_CREATED");
        assert_eq!(records[0].ip_address.as_deref(), Some(CLIENT_IP));
        assert_eq!(records[0].user_agent, CLIENT_AGENT);
    }

    #[tokio::test]
    async fn pairing_while_the_mesh_is_off_is_a_conflict() {
        let db = mock_db()
            .append_query_results(vec![vec![network_config(false, false)]])
            .into_connection();
        let app = simple_app(
            db,
            Arc::new(AllowSensitiveActions),
            Arc::new(RecordingAuditLogger::default()),
        );
        let (status, _, body) = send(
            app,
            "POST",
            "/nodes/pairings",
            Some(create_body()),
            Some(admin()),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["title"], "WireGuard Mesh Off");
    }

    #[tokio::test]
    async fn cancelling_a_pairing_revokes_its_token_and_is_audited() {
        let db = mock_db()
            .append_query_results(vec![vec![pairing(3, "waiting")]])
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();
        let mut revoked = enrollment_token(11);
        revoked.revoked_at = Some(chrono::Utc::now());
        let tokens_db = mock_db()
            .append_query_results(vec![vec![enrollment_token(11)]])
            .append_query_results(vec![vec![revoked]])
            .into_connection();
        let audit = Arc::new(RecordingAuditLogger::default());
        let app = app(
            db,
            tokens_db,
            temps_core::AppSettings::default(),
            Arc::new(AllowSensitiveActions),
            audit.clone(),
        );
        let (status, _, body) = send(app, "DELETE", "/nodes/pairings/3", None, Some(admin())).await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

        let records = audit.records();
        assert_eq!(records.len(), 1, "{records:?}");
        assert_eq!(records[0].operation, "NODE_PAIRING_CANCELLED");
        assert_eq!(records[0].ip_address.as_deref(), Some(CLIENT_IP));
        assert_eq!(records[0].user_agent, CLIENT_AGENT);
    }

    #[tokio::test]
    async fn cancelling_an_unknown_pairing_is_not_found() {
        let db = mock_db()
            .append_query_results(vec![Vec::<node_pairings::Model>::new()])
            .into_connection();
        let audit = Arc::new(RecordingAuditLogger::default());
        let app = simple_app(db, Arc::new(AllowSensitiveActions), audit.clone());
        let (status, _, body) = send(app, "DELETE", "/nodes/pairings/9", None, Some(admin())).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
        assert!(audit.records().is_empty());
    }

    #[tokio::test]
    async fn cancelling_a_finished_pairing_is_a_conflict() {
        let db = mock_db()
            .append_query_results(vec![vec![pairing(3, "completed")]])
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .into_connection();
        let audit = Arc::new(RecordingAuditLogger::default());
        let app = simple_app(db, Arc::new(AllowSensitiveActions), audit.clone());
        let (status, _, body) = send(app, "DELETE", "/nodes/pairings/3", None, Some(admin())).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(
            body["detail"],
            "This pairing is completed; nothing to cancel."
        );
        assert!(audit.records().is_empty());
    }
}
