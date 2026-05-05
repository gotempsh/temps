//! Node Registration Handlers
//!
//! Internal API endpoints for worker nodes to register with the control plane
//! and send heartbeats. These endpoints use token-based authentication
//! (not the regular user auth) — the node presents the registration token
//! which is verified against the hashed token stored in the nodes table.

use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use sea_orm::{DatabaseConnection, EntityTrait};
use serde::{Deserialize, Serialize};
use sha2::Digest;
use temps_auth::{permission_guard, RequireAuth};
use temps_config::ConfigService;
use tracing::{error, info, warn};
use utoipa::{OpenApi, ToSchema};

use crate::handlers::types::AppState;
use crate::services::node_service::{
    HeartbeatRequest, NodeError, NodeService, RegisterNodeRequest,
};
use temps_core::problemdetails::{self, Problem};
use temps_deployer::ContainerDeployer;

/// App state for node registration handlers
pub struct NodeAppState {
    pub node_service: Arc<NodeService>,
    pub db: Arc<DatabaseConnection>,
    pub config_service: Arc<ConfigService>,
    pub encryption_service: Arc<temps_core::EncryptionService>,
}

#[derive(Deserialize, ToSchema)]
pub struct RegisterNodeApiRequest {
    /// Unique name for this node
    pub name: String,
    /// Registration token (plaintext, will be hashed before storage)
    pub token: String,
    /// Join token to authorize this registration (must match the token generated in Settings)
    pub join_token: Option<String>,
    /// Node's reachable address (e.g., "10.100.0.2" or "192.168.1.50")
    pub address: String,
    /// Private/WireGuard address for inter-node communication
    pub private_address: String,
    /// Public endpoint for WireGuard (e.g., "203.0.113.1:51820")
    pub public_endpoint: Option<String>,
    /// WireGuard public key
    pub wg_public_key: Option<String>,
    /// Node role (default: "worker")
    pub role: Option<String>,
    /// Labels for scheduling (e.g., {"region": "us-east", "gpu": "true"})
    pub labels: Option<serde_json::Value>,
    /// X25519 public key for ECIES certificate encryption (base64-encoded, edge nodes only)
    pub edge_public_key: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct RegisterNodeResponse {
    pub id: i32,
    pub name: String,
    pub status: String,
    pub message: String,
}

#[derive(Deserialize, ToSchema)]
pub struct HeartbeatApiRequest {
    /// Resource capacity/usage info as JSON (cpu_usage, memory_usage, etc.)
    pub capacity: Option<serde_json::Value>,
    /// Updated node labels for scheduling (allows runtime label changes).
    pub labels: Option<serde_json::Value>,
    /// Container inventory for reconciliation (sent on first heartbeat after agent startup).
    /// Each entry has `container_id` and `container_name` of temps-managed containers.
    pub containers: Option<Vec<ContainerInventoryItem>>,
}

/// A container reported by the agent during heartbeat reconciliation.
#[derive(Deserialize, ToSchema)]
pub struct ContainerInventoryItem {
    /// Docker container ID
    pub container_id: String,
    /// Docker container name
    pub container_name: String,
}

#[derive(Serialize, ToSchema)]
pub struct HeartbeatResponse {
    pub status: String,
    pub message: String,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct NodeInfoResponse {
    pub id: i32,
    pub name: String,
    pub address: String,
    pub private_address: String,
    pub role: String,
    pub status: String,
    pub labels: serde_json::Value,
    /// Resource capacity/usage metrics from the latest heartbeat
    pub capacity: serde_json::Value,
    pub last_heartbeat: Option<String>,
    pub created_at: String,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct NodeListResponse {
    pub nodes: Vec<NodeInfoResponse>,
    pub total: usize,
}

/// A container running on a specific node, enriched with project/environment context.
#[derive(Serialize, Deserialize, ToSchema)]
pub struct NodeContainerResponse {
    pub container_id: String,
    pub container_name: String,
    pub image_name: String,
    pub status: String,
    pub created_at: String,
    pub deployment_id: i32,
    pub project_id: i32,
    pub project_name: String,
    pub environment_id: i32,
    pub environment_name: String,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct NodeContainerListResponse {
    pub containers: Vec<NodeContainerResponse>,
    pub total: usize,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct DrainNodeResponse {
    pub id: i32,
    pub name: String,
    pub status: String,
    pub affected_environments: usize,
    pub message: String,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct RemoveNodeResponse {
    pub id: i32,
    pub message: String,
}

/// Progress of a node drain operation.
#[derive(Serialize, Deserialize, ToSchema)]
pub struct DrainStatusResponse {
    pub node_id: i32,
    pub node_name: String,
    pub status: String,
    /// Number of containers still on this node
    pub remaining_containers: usize,
    /// Whether the drain is complete (all containers migrated)
    pub drain_complete: bool,
    /// Can the node be safely removed?
    pub can_remove: bool,
    pub message: String,
}

/// Response after undraining (reactivating) a node.
#[derive(Serialize, Deserialize, ToSchema)]
pub struct UndrainNodeResponse {
    pub id: i32,
    pub name: String,
    pub status: String,
    pub message: String,
}

/// A single route entry for edge CDN nodes.
#[derive(Serialize, Deserialize, ToSchema)]
pub struct EdgeRouteEntry {
    pub domain: String,
    pub is_static: bool,
    /// Whether this domain uses wildcard matching (e.g. `*.localho.st`).
    #[serde(default)]
    pub is_wildcard: bool,
    pub project_id: Option<i32>,
    pub environment_id: Option<i32>,
}

/// An encrypted TLS certificate bundle for an edge node.
#[derive(Serialize, Deserialize, ToSchema)]
pub struct EdgeCertBundle {
    pub domain: String,
    /// Base64-encoded AES-256-GCM ciphertext of (cert_pem + "\n" + key_pem)
    pub ciphertext: String,
    /// Base64-encoded 12-byte nonce
    pub nonce: String,
    /// SHA-256 hex fingerprint of the certificate PEM (for change detection)
    pub fingerprint: String,
}

/// Encrypted certificate payload in the edge routes response.
#[derive(Serialize, Deserialize, ToSchema)]
pub struct EdgeCertificates {
    /// Base64-encoded ephemeral X25519 public key (for ECDH)
    pub ephemeral_public_key: String,
    pub bundles: Vec<EdgeCertBundle>,
}

/// Response from `GET /api/internal/edge/routes`.
#[derive(Serialize, Deserialize, ToSchema)]
pub struct EdgeRoutesResponse {
    pub routes: Vec<EdgeRouteEntry>,
    pub version: u64,
    /// Encrypted TLS certificates (present only if the edge node has a public key registered)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub certificates: Option<EdgeCertificates>,
}

/// S3 credentials distributed to agents for backup/restore operations.
#[derive(Serialize, Deserialize, ToSchema)]
pub struct S3CredentialsResponse {
    pub access_key_id: String,
    pub secret_key: String,
    pub region: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    pub bucket_name: String,
    pub force_path_style: bool,
}

#[derive(OpenApi)]
#[openapi(
    paths(
        register_node,
        node_heartbeat,
        get_s3_credentials,
        crate::handlers::network::list_peers,
        admin_list_nodes,
        admin_get_node,
        admin_list_node_containers,
        admin_drain_node,
        admin_undrain_node,
        admin_remove_node,
        admin_drain_status,
    ),
    components(schemas(
        RegisterNodeApiRequest,
        RegisterNodeResponse,
        HeartbeatApiRequest,
        HeartbeatResponse,
        S3CredentialsResponse,
        crate::handlers::network::PeerEntry,
        crate::handlers::network::AllocEntry,
        crate::handlers::network::PeerListResponse,
        NodeInfoResponse,
        NodeListResponse,
        NodeContainerResponse,
        NodeContainerListResponse,
        DrainNodeResponse,
        UndrainNodeResponse,
        RemoveNodeResponse,
        DrainStatusResponse,
    )),
    info(
        title = "Node Registration API",
        description = "Internal API for worker nodes to register and send heartbeats to the control plane.",
        version = "1.0.0"
    )
)]
pub struct NodesApiDoc;

/// Configure agent-facing node routes (bearer token auth via NodeAppState).
/// These are mounted separately from the plugin system.
pub fn configure_routes() -> Router<Arc<NodeAppState>> {
    Router::new()
        .route("/internal/nodes/register", post(register_node))
        .route("/internal/nodes/{node_id}/heartbeat", post(node_heartbeat))
        .route(
            "/internal/nodes/{node_id}/s3-credentials/{s3_source_id}",
            get(get_s3_credentials),
        )
        .route(
            "/internal/nodes/{node_id}/network/peers",
            get(crate::handlers::network::list_peers),
        )
        .route("/internal/edge/routes", get(edge_routes))
}

/// Configure UI-facing admin node routes (session auth via RequireAuth).
/// These are registered through the plugin system's AppState.
pub fn configure_admin_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/internal/nodes", get(admin_list_nodes))
        .route(
            "/internal/nodes/{node_id}",
            get(admin_get_node).delete(admin_remove_node),
        )
        .route(
            "/internal/nodes/{node_id}/containers",
            get(admin_list_node_containers),
        )
        .route(
            "/internal/nodes/{node_id}/drain",
            get(admin_drain_status)
                .post(admin_drain_node)
                .delete(admin_undrain_node),
        )
        // Edge analytics proxy routes — forwards queries to edge nodes
        .route(
            "/internal/edge/analytics/overview",
            get(proxy_edge_analytics_overview),
        )
        .route(
            "/internal/edge/analytics/domains",
            get(proxy_edge_analytics_domains),
        )
        .route(
            "/internal/edge/analytics/assets",
            get(proxy_edge_analytics_assets),
        )
        .route(
            "/internal/edge/analytics/timeseries",
            get(proxy_edge_analytics_timeseries),
        )
        .route("/internal/edge/nodes", get(list_edge_nodes))
}

/// SHA-256 hash a token string
fn sha256_hash(token: &str) -> String {
    let digest = sha2::Sha256::digest(token.as_bytes());
    format!("{:x}", digest)
}

/// Constant-time comparison of two byte slices to prevent timing attacks on token hashes.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Validate that an api_address is a safe host:port pair.
/// Rejects cloud metadata IPs, loopback, and link-local ranges to prevent SSRF.
fn is_safe_api_address(addr: &str) -> bool {
    // Must be host:port format
    let Some((host, port_str)) = addr.rsplit_once(':') else {
        return false;
    };
    // Port must be a valid number
    if port_str.parse::<u16>().is_err() {
        return false;
    }
    // Reject if host contains path separators (injection attempt)
    if host.contains('/') || host.contains('@') || host.contains('#') {
        return false;
    }
    // Check IP-based addresses against blocklist
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        use std::net::IpAddr;
        match ip {
            IpAddr::V4(v4) => {
                // Block cloud metadata (169.254.169.254), link-local, loopback
                if v4.is_loopback()
                    || v4.is_link_local()
                    || v4.is_broadcast()
                    || v4.is_unspecified()
                {
                    return false;
                }
                // Block AWS/GCP/Azure metadata endpoint specifically
                let octets = v4.octets();
                if octets[0] == 169 && octets[1] == 254 {
                    return false;
                }
            }
            IpAddr::V6(v6) => {
                if v6.is_loopback() || v6.is_unspecified() {
                    return false;
                }
            }
        }
    }
    true
}

/// Extract and verify the bearer token from request headers.
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

/// Register a new worker node or reconnect an existing one
#[utoipa::path(
    tag = "Nodes",
    post,
    path = "/internal/nodes/register",
    request_body = RegisterNodeApiRequest,
    responses(
        (status = 201, description = "Node registered successfully", body = RegisterNodeResponse),
        (status = 200, description = "Node reconnected successfully", body = RegisterNodeResponse),
        (status = 400, description = "Validation error", ),
        (status = 500, description = "Internal server error", )
    )
)]
async fn register_node(
    State(app_state): State<Arc<NodeAppState>>,
    Json(request): Json<RegisterNodeApiRequest>,
) -> Result<impl IntoResponse, Problem> {
    // Validate join token against the stored hash in settings
    let settings = app_state.config_service.get_settings().await.map_err(|e| {
        error!("Failed to read settings for join token validation: {}", e);
        problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
            .with_title("Internal Server Error")
            .with_detail("Failed to validate join token")
    })?;

    match settings.multi_node.join_token_hash {
        Some(ref stored_hash) => {
            // Join token is configured — require it
            match &request.join_token {
                Some(provided_token) => {
                    let provided_hash = sha256_hash(provided_token);
                    if !constant_time_eq(provided_hash.as_bytes(), stored_hash.as_bytes()) {
                        warn!(
                            "Node registration rejected: invalid join token for node '{}'",
                            request.name
                        );
                        return Err(problemdetails::new(StatusCode::FORBIDDEN)
                            .with_title("Invalid Join Token")
                            .with_detail("The provided join token is invalid"));
                    }
                }
                None => {
                    warn!(
                        "Node registration rejected: missing join token for node '{}'",
                        request.name
                    );
                    return Err(problemdetails::new(StatusCode::FORBIDDEN)
                        .with_title("Join Token Required")
                        .with_detail("A join token is required to register a node. Generate one in Settings > Worker Nodes."));
                }
            }
        }
        None => {
            // No join token configured — block all registrations
            warn!("Node registration rejected: multi-node not enabled (no join token configured) for node '{}'", request.name);
            return Err(problemdetails::new(StatusCode::FORBIDDEN)
                .with_title("Registration Disabled")
                .with_detail("Node registration is not enabled. Generate a join token in Settings > Worker Nodes to enable multi-node."));
        }
    }

    let token_hash = sha256_hash(&request.token);

    // Encrypt the plaintext token so the control plane can authenticate
    // with the agent for remote deployments
    let token_encrypted = app_state
        .encryption_service
        .encrypt(request.token.as_bytes())
        .map_err(|e| {
            error!("Failed to encrypt node token: {}", e);
            problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Internal Server Error")
                .with_detail("Failed to process node registration")
        })?;

    let register_request = RegisterNodeRequest {
        name: request.name.trim().to_string(),
        token_hash,
        token_encrypted: Some(token_encrypted),
        address: request.address.trim().to_string(),
        private_address: request.private_address.trim().to_string(),
        public_endpoint: request.public_endpoint,
        wg_public_key: request.wg_public_key,
        role: request.role.unwrap_or_else(|| "worker".to_string()),
        labels: request.labels.unwrap_or(serde_json::json!({})),
        edge_public_key: request.edge_public_key,
    };

    let node = app_state
        .node_service
        .register(register_request)
        .await
        .map_err(Problem::from)?;

    info!(node_id = node.id, name = %node.name, "Node registered successfully");

    // ── Multi-host networking: best-effort overlay setup ──
    //
    // Persist the node's reachable underlay address (private_address is what
    // other nodes will tunnel to via VXLAN), then ask the allocator to
    // assign a compute_cidr from the cluster pool. Both are best-effort —
    // failures here MUST NOT break the join flow. The agent's network_sync
    // loop polls /network/peers indefinitely and will pick up the
    // allocation as soon as it lands, so a transient failure self-heals.
    persist_underlay_address(
        app_state.db.as_ref(),
        node.id,
        node.private_address.as_str(),
    )
    .await;
    allocate_overlay_cidr(app_state.db.clone(), node.id).await;

    Ok((
        StatusCode::CREATED,
        Json(RegisterNodeResponse {
            id: node.id,
            name: node.name,
            status: node.status,
            message: "Node registered successfully. Send heartbeats to stay active.".to_string(),
        }),
    ))
}

/// Set `nodes.underlay_address` to the value other nodes will use to reach
/// this one over the overlay. Best-effort: failures are logged and
/// swallowed because the operator can fix manually and the agent will
/// pick up the change on its next poll.
async fn persist_underlay_address(db: &sea_orm::DatabaseConnection, node_id: i32, underlay: &str) {
    use sea_orm::{sea_query::Expr, ColumnTrait, EntityTrait, QueryFilter};
    use temps_entities::nodes;

    let result = nodes::Entity::update_many()
        .col_expr(
            nodes::Column::UnderlayAddress,
            Expr::value(Some(underlay.to_string())),
        )
        .filter(nodes::Column::Id.eq(node_id))
        .exec(db)
        .await;
    match result {
        Ok(_) => info!(node_id, underlay, "underlay_address set"),
        Err(e) => warn!(
            node_id,
            "failed to set underlay_address (overlay may be delayed): {}", e
        ),
    }
}

/// Ask the allocator for a compute_cidr. Treat AlreadyAllocated as success
/// (re-registration after a restart). Treat any other error as a
/// non-fatal warning — the join flow stays successful so the operator
/// isn't blocked from running deployments while overlay networking
/// converges in the background.
async fn allocate_overlay_cidr(db: std::sync::Arc<sea_orm::DatabaseConnection>, node_id: i32) {
    use temps_network::allocator::{AllocatorError, ComputeNetworkAllocator, PostgresAllocator};

    let allocator = PostgresAllocator::new(db);
    match allocator.allocate_for_node(node_id).await {
        Ok(alloc) => info!(
            node_id,
            cidr = %alloc.compute_cidr,
            "compute_cidr auto-allocated on join"
        ),
        Err(AllocatorError::AlreadyAllocated { existing, .. }) => {
            info!(node_id, %existing, "compute_cidr already present (re-registration)");
        }
        Err(AllocatorError::UnderlayMissing { .. }) => {
            warn!(
                node_id,
                "underlay_address missing during allocation; agent sync will retry"
            );
        }
        Err(e) => warn!(
            node_id,
            "failed to allocate compute_cidr (overlay deferred to next agent poll): {}", e
        ),
    }
}

/// Receive a heartbeat from a worker node
#[utoipa::path(
    tag = "Nodes",
    post,
    path = "/internal/nodes/{node_id}/heartbeat",
    params(
        ("node_id" = i32, Path, description = "Node ID")
    ),
    request_body = HeartbeatApiRequest,
    responses(
        (status = 200, description = "Heartbeat received", body = HeartbeatResponse),
        (status = 401, description = "Unauthorized", ),
        (status = 404, description = "Node not found", ),
        (status = 500, description = "Internal server error", )
    )
)]
async fn node_heartbeat(
    State(app_state): State<Arc<NodeAppState>>,
    headers: HeaderMap,
    Path(node_id): Path<i32>,
    Json(request): Json<HeartbeatApiRequest>,
) -> Result<impl IntoResponse, Problem> {
    // Verify the node's token
    let token = extract_bearer_token(&headers)?;

    // Get the node and verify token hash
    let node = app_state
        .node_service
        .get_by_id(node_id)
        .await
        .map_err(Problem::from)?;

    let token_hash = sha256_hash(&token);
    if !constant_time_eq(node.token_hash.as_bytes(), token_hash.as_bytes()) {
        warn!(node_id, "Invalid heartbeat token");
        return Err(problemdetails::new(StatusCode::UNAUTHORIZED)
            .with_title("Invalid Token")
            .with_detail(format!("Invalid authentication token for node {}", node_id)));
    }

    // Capture previous status before the heartbeat updates it
    let was_offline = node.status == "offline";

    let heartbeat = HeartbeatRequest {
        capacity: request.capacity.unwrap_or(serde_json::json!({})),
        labels: request.labels,
    };

    app_state
        .node_service
        .heartbeat(node_id, heartbeat)
        .await
        .map_err(Problem::from)?;

    // Reconcile container state when the agent sends its inventory.
    // This happens on the first heartbeat after agent startup/reconnect.
    if let Some(containers) = request.containers {
        let container_ids: Vec<String> =
            containers.iter().map(|c| c.container_id.clone()).collect();

        info!(
            node_id,
            container_count = container_ids.len(),
            was_offline,
            "Received container inventory from agent, reconciling"
        );

        match app_state
            .node_service
            .reconcile_containers(node_id, &container_ids)
            .await
        {
            Ok(stale_count) => {
                if stale_count > 0 {
                    info!(
                        node_id,
                        stale_count,
                        "Reconciliation: marked {} stale DB record(s) as deleted",
                        stale_count
                    );
                }
            }
            Err(e) => {
                error!(node_id, "Container reconciliation failed: {}", e);
            }
        }
    }

    Ok(Json(HeartbeatResponse {
        status: "ok".to_string(),
        message: "Heartbeat received".to_string(),
    }))
}

/// Get decrypted S3 credentials for a backup/restore operation.
///
/// Agents call this endpoint to receive the S3 credentials they need to upload
/// or download backups. The credentials are decrypted from the stored S3 source
/// and returned over the authenticated TLS/WireGuard channel.
#[utoipa::path(
    tag = "Nodes",
    get,
    path = "/internal/nodes/{node_id}/s3-credentials/{s3_source_id}",
    params(
        ("node_id" = i32, Path, description = "Node ID"),
        ("s3_source_id" = i32, Path, description = "S3 source ID")
    ),
    responses(
        (status = 200, description = "S3 credentials", body = S3CredentialsResponse),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "S3 source not found"),
        (status = 500, description = "Internal server error")
    )
)]
async fn get_s3_credentials(
    State(app_state): State<Arc<NodeAppState>>,
    headers: HeaderMap,
    Path((node_id, s3_source_id)): Path<(i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    // Verify the node's token
    let token = extract_bearer_token(&headers)?;
    let node = app_state
        .node_service
        .get_by_id(node_id)
        .await
        .map_err(Problem::from)?;

    let token_hash = sha256_hash(&token);
    if !constant_time_eq(node.token_hash.as_bytes(), token_hash.as_bytes()) {
        warn!(node_id, "Invalid token for S3 credentials request");
        return Err(problemdetails::new(StatusCode::UNAUTHORIZED)
            .with_title("Invalid Token")
            .with_detail(format!("Invalid authentication token for node {}", node_id)));
    }

    // Look up the S3 source
    let s3_source = temps_entities::s3_sources::Entity::find_by_id(s3_source_id)
        .one(app_state.db.as_ref())
        .await
        .map_err(|e| {
            error!("Failed to look up S3 source {}: {}", s3_source_id, e);
            problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Database Error")
                .with_detail(format!("Failed to look up S3 source: {}", e))
        })?
        .ok_or_else(|| {
            problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("S3 Source Not Found")
                .with_detail(format!("S3 source {} not found", s3_source_id))
        })?;

    // Decrypt credentials
    let access_key_id = app_state
        .encryption_service
        .decrypt_string(&s3_source.access_key_id)
        .map_err(|e| {
            error!(
                "Failed to decrypt access key for S3 source {}: {}",
                s3_source_id, e
            );
            problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Decryption Error")
                .with_detail("Failed to decrypt S3 credentials")
        })?;

    let secret_key = app_state
        .encryption_service
        .decrypt_string(&s3_source.secret_key)
        .map_err(|e| {
            error!(
                "Failed to decrypt secret key for S3 source {}: {}",
                s3_source_id, e
            );
            problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Decryption Error")
                .with_detail("Failed to decrypt S3 credentials")
        })?;

    info!(
        "Distributed S3 credentials for source {} to node {} ({})",
        s3_source_id, node_id, node.name
    );

    Ok(Json(S3CredentialsResponse {
        access_key_id,
        secret_key,
        region: s3_source.region,
        endpoint: s3_source.endpoint,
        bucket_name: s3_source.bucket_name,
        force_path_style: s3_source.force_path_style.unwrap_or(true),
    }))
}

/// Return the route table for edge CDN nodes.
///
/// Lists all active environment domains with their project/environment IDs
/// and whether they serve static files. Edge nodes poll this endpoint to
/// know which domains they should handle.
#[utoipa::path(
    tag = "Nodes",
    get,
    path = "/internal/edge/routes",
    responses(
        (status = 200, description = "Edge route table", body = EdgeRoutesResponse),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    )
)]
async fn edge_routes(
    State(app_state): State<Arc<NodeAppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, Problem> {
    // Verify bearer token belongs to a registered node
    let token = extract_bearer_token(&headers)?;
    let token_hash = sha256_hash(&token);

    use sea_orm::{ColumnTrait, QueryFilter};
    use temps_entities::nodes;

    let node = nodes::Entity::find()
        .filter(nodes::Column::TokenHash.eq(&token_hash))
        .one(app_state.db.as_ref())
        .await
        .map_err(|e| {
            error!("Edge routes: DB error verifying token: {}", e);
            problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Database Error")
                .with_detail("Failed to verify edge token")
        })?
        .ok_or_else(|| {
            problemdetails::new(StatusCode::UNAUTHORIZED)
                .with_title("Invalid Token")
                .with_detail("No node found with this token")
        })?;

    info!(
        "Edge node {} ({}) requested route table",
        node.id, node.name
    );

    // Query all active environment domains with their environments and deployments
    use temps_entities::{
        custom_routes, deployments, environment_domains, environments, project_custom_domains,
    };

    let domains: Vec<(environment_domains::Model, Option<environments::Model>)> =
        environment_domains::Entity::find()
            .find_also_related(environments::Entity)
            .all(app_state.db.as_ref())
            .await
            .map_err(|e| {
                error!("Edge routes: failed to query domains: {}", e);
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Database Error")
                    .with_detail("Failed to query environment domains")
            })?;

    let mut routes = Vec::with_capacity(domains.len());

    // 1. Environment domains (explicit per-environment domains)
    for (domain_entry, env_opt) in &domains {
        let env = match env_opt {
            Some(e) => e,
            None => continue,
        };

        // Check if the current deployment is static
        let is_static = if let Some(deploy_id) = env.current_deployment_id {
            deployments::Entity::find_by_id(deploy_id)
                .one(app_state.db.as_ref())
                .await
                .ok()
                .flatten()
                .map(|d| d.static_dir_location.is_some())
                .unwrap_or(false)
        } else {
            false
        };

        routes.push(EdgeRouteEntry {
            domain: domain_entry.domain.clone(),
            is_static,
            is_wildcard: false,
            project_id: Some(env.project_id),
            environment_id: Some(env.id),
        });
    }

    // 2. Preview domain routes: {subdomain}.{preview_domain} for all active environments
    //    (mirrors Section 4 of the control-plane route table)
    {
        use temps_entities::settings;

        let preview_domain = settings::Entity::find()
            .one(app_state.db.as_ref())
            .await
            .ok()
            .flatten()
            .and_then(|s| {
                s.data
                    .get("preview_domain")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            })
            .unwrap_or_else(|| "localho.st".to_string());

        let all_envs = environments::Entity::find()
            .filter(environments::Column::Subdomain.is_not_null())
            .filter(environments::Column::CurrentDeploymentId.is_not_null())
            .filter(environments::Column::DeletedAt.is_null())
            .all(app_state.db.as_ref())
            .await
            .map_err(|e| {
                error!(
                    "Edge routes: failed to query environments for preview domains: {}",
                    e
                );
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Database Error")
                    .with_detail("Failed to query environments")
            })?;

        for env in &all_envs {
            let full_domain = format!("{}.{}", env.subdomain, preview_domain);
            // Skip if already added from environment_domains
            if routes.iter().any(|r| r.domain == full_domain) {
                continue;
            }

            let is_static = if let Some(deploy_id) = env.current_deployment_id {
                deployments::Entity::find_by_id(deploy_id)
                    .one(app_state.db.as_ref())
                    .await
                    .ok()
                    .flatten()
                    .map(|d| d.static_dir_location.is_some())
                    .unwrap_or(false)
            } else {
                false
            };

            routes.push(EdgeRouteEntry {
                domain: full_domain,
                is_static,
                is_wildcard: false,
                project_id: Some(env.project_id),
                environment_id: Some(env.id),
            });
        }
    }

    // 3. Custom routes (including wildcards like *.localho.st)
    let custom_routes_data = custom_routes::Entity::find()
        .filter(custom_routes::Column::Enabled.eq(true))
        .all(app_state.db.as_ref())
        .await
        .map_err(|e| {
            error!("Edge routes: failed to query custom routes: {}", e);
            problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Database Error")
                .with_detail("Failed to query custom routes")
        })?;

    for custom_route in &custom_routes_data {
        routes.push(EdgeRouteEntry {
            domain: custom_route.domain.clone(),
            is_static: false,
            is_wildcard: custom_route.domain.starts_with("*."),
            project_id: None,
            environment_id: None,
        });
    }

    // 3. Project custom domains (custom domains mapped to environments)
    let custom_domains = project_custom_domains::Entity::find()
        .all(app_state.db.as_ref())
        .await
        .map_err(|e| {
            error!("Edge routes: failed to query project custom domains: {}", e);
            problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Database Error")
                .with_detail("Failed to query project custom domains")
        })?;

    for custom_domain in &custom_domains {
        // Skip domains already added from environment_domains
        if routes.iter().any(|r| r.domain == custom_domain.domain) {
            continue;
        }
        routes.push(EdgeRouteEntry {
            domain: custom_domain.domain.clone(),
            is_static: false,
            is_wildcard: false,
            project_id: Some(custom_domain.project_id),
            environment_id: Some(custom_domain.environment_id),
        });
    }

    // Encrypt TLS certificates for this edge node (if it has a public key)
    // Only edge nodes should receive TLS private keys — never workers
    let certificates = 'cert_block: {
        if node.role != "edge" {
            break 'cert_block None;
        }
        let edge_pk = match node.edge_public_key {
            Some(ref pk) => pk,
            None => break 'cert_block None,
        };

        use temps_entities::domains;

        let active_domains = domains::Entity::find()
            .filter(domains::Column::Status.eq("active"))
            .all(app_state.db.as_ref())
            .await
            .map_err(|e| {
                error!("Edge routes: failed to query domains for certs: {}", e);
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Database Error")
                    .with_detail("Failed to query domain certificates")
            })?;

        // Create one encryption session per sync (single ephemeral key, forward secrecy)
        let session = match temps_core::ecies::EncryptionSession::new(edge_pk) {
            Ok(s) => s,
            Err(e) => {
                warn!(
                    "Edge routes: invalid edge public key for node {}: {}",
                    node.id, e
                );
                break 'cert_block None;
            }
        };

        let mut bundles = Vec::new();

        for domain in &active_domains {
            if let (Some(cert_pem), Some(encrypted_key_pem)) =
                (&domain.certificate, &domain.private_key)
            {
                // Decrypt the private key stored in the DB (it's AES-encrypted by EncryptionService)
                let key_pem = match app_state
                    .encryption_service
                    .decrypt_string(encrypted_key_pem)
                {
                    Ok(k) => k,
                    Err(e) => {
                        warn!(
                            "Edge routes: failed to decrypt private key for domain {}: {}",
                            domain.domain, e
                        );
                        continue;
                    }
                };

                // Combine cert + key into a single payload
                let payload = format!("{}\n{}", cert_pem, key_pem);
                let fingerprint = temps_core::ecies::cert_fingerprint(cert_pem);

                match session.encrypt(payload.as_bytes()) {
                    Ok(bundle) => {
                        bundles.push(EdgeCertBundle {
                            domain: domain.domain.clone(),
                            ciphertext: bundle.ciphertext,
                            nonce: bundle.nonce,
                            fingerprint,
                        });
                    }
                    Err(e) => {
                        warn!(
                            "Edge routes: failed to encrypt cert for domain {}: {}",
                            domain.domain, e
                        );
                    }
                }
            }
        }

        if bundles.is_empty() {
            None
        } else {
            Some(EdgeCertificates {
                ephemeral_public_key: session.ephemeral_public_key().to_string(),
                bundles,
            })
        }
    };

    // Use a simple counter based on the current timestamp as version
    let version = chrono::Utc::now().timestamp() as u64;

    Ok(Json(EdgeRoutesResponse {
        routes,
        version,
        certificates,
    }))
}

/// List all registered nodes (admin — session auth via RequireAuth)
#[utoipa::path(
    tag = "Nodes",
    get,
    path = "/internal/nodes",
    responses(
        (status = 200, description = "List of nodes", body = NodeListResponse),
        (status = 401, description = "Unauthorized", ),
        (status = 500, description = "Internal server error", )
    ),
    security(("bearer_auth" = []))
)]
async fn admin_list_nodes(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsRead);
    let nodes = app_state
        .node_service
        .list_all()
        .await
        .map_err(Problem::from)?;

    let total = nodes.len();
    let node_responses: Vec<NodeInfoResponse> = nodes
        .into_iter()
        .map(|n| NodeInfoResponse {
            id: n.id,
            name: n.name,
            address: n.address,
            private_address: n.private_address,
            role: n.role,
            status: n.status,
            labels: n.labels,
            capacity: n.capacity,
            last_heartbeat: n.last_heartbeat.map(|t| t.to_rfc3339()),
            created_at: n.created_at.to_rfc3339(),
        })
        .collect();

    Ok(Json(NodeListResponse {
        nodes: node_responses,
        total,
    }))
}

/// Get a specific node by ID (admin — session auth via RequireAuth)
#[utoipa::path(
    tag = "Nodes",
    get,
    path = "/internal/nodes/{node_id}",
    params(
        ("node_id" = i32, Path, description = "Node ID")
    ),
    responses(
        (status = 200, description = "Node details", body = NodeInfoResponse),
        (status = 401, description = "Unauthorized", ),
        (status = 404, description = "Node not found", ),
        (status = 500, description = "Internal server error", )
    ),
    security(("bearer_auth" = []))
)]
async fn admin_get_node(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Path(node_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsRead);
    let node = app_state
        .node_service
        .get_by_id(node_id)
        .await
        .map_err(Problem::from)?;

    Ok(Json(NodeInfoResponse {
        id: node.id,
        name: node.name,
        address: node.address,
        private_address: node.private_address,
        role: node.role,
        status: node.status,
        labels: node.labels,
        capacity: node.capacity,
        last_heartbeat: node.last_heartbeat.map(|t| t.to_rfc3339()),
        created_at: node.created_at.to_rfc3339(),
    }))
}

/// List all containers running on a specific node
#[utoipa::path(
    tag = "Nodes",
    get,
    path = "/internal/nodes/{node_id}/containers",
    params(
        ("node_id" = i32, Path, description = "Node ID")
    ),
    responses(
        (status = 200, description = "Containers on this node", body = NodeContainerListResponse),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Node not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
async fn admin_list_node_containers(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Path(node_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsRead);
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

    // Verify the node exists
    let _node = app_state
        .node_service
        .get_by_id(node_id)
        .await
        .map_err(Problem::from)?;

    // Query containers for this node, joining with deployments, projects, and environments
    let rows: Vec<(
        temps_entities::deployment_containers::Model,
        Option<temps_entities::deployments::Model>,
    )> = temps_entities::deployment_containers::Entity::find()
        .filter(temps_entities::deployment_containers::Column::NodeId.eq(node_id))
        .filter(temps_entities::deployment_containers::Column::DeletedAt.is_null())
        .find_also_related(temps_entities::deployments::Entity)
        .all(app_state.db.as_ref())
        .await
        .map_err(|e| {
            error!("Failed to query containers for node {}: {}", node_id, e);
            problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Internal Server Error")
                .with_detail("Failed to query node containers")
        })?;

    // Collect unique project and environment IDs
    let mut project_ids = std::collections::HashSet::new();
    let mut environment_ids = std::collections::HashSet::new();
    for (_, deployment) in &rows {
        if let Some(d) = deployment {
            project_ids.insert(d.project_id);
            environment_ids.insert(d.environment_id);
        }
    }

    // Batch-fetch project names
    let projects: std::collections::HashMap<i32, String> = temps_entities::projects::Entity::find()
        .filter(temps_entities::projects::Column::Id.is_in(project_ids))
        .all(app_state.db.as_ref())
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|p| (p.id, p.name))
        .collect();

    // Batch-fetch environment names
    let environments: std::collections::HashMap<i32, String> =
        temps_entities::environments::Entity::find()
            .filter(temps_entities::environments::Column::Id.is_in(environment_ids))
            .all(app_state.db.as_ref())
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|e| (e.id, e.name))
            .collect();

    let containers: Vec<NodeContainerResponse> = rows
        .into_iter()
        .filter_map(|(container, deployment)| {
            let d = deployment?;
            Some(NodeContainerResponse {
                container_id: container.container_id,
                container_name: container.container_name,
                image_name: container.image_name.unwrap_or_default(),
                status: container.status.unwrap_or_else(|| "unknown".to_string()),
                created_at: container.created_at.to_rfc3339(),
                deployment_id: d.id,
                project_id: d.project_id,
                project_name: projects
                    .get(&d.project_id)
                    .cloned()
                    .unwrap_or_else(|| format!("project-{}", d.project_id)),
                environment_id: d.environment_id,
                environment_name: environments
                    .get(&d.environment_id)
                    .cloned()
                    .unwrap_or_else(|| format!("env-{}", d.environment_id)),
            })
        })
        .collect();

    let total = containers.len();
    Ok(Json(NodeContainerListResponse { containers, total }))
}

/// Create a `RemoteNodeDeployer` for stopping containers on a worker node.
/// Returns `None` if the node has no encrypted token or decryption fails (best-effort).
fn create_remote_deployer(
    node: &temps_entities::nodes::Model,
    encryption_service: &temps_core::EncryptionService,
) -> Option<Arc<dyn ContainerDeployer>> {
    let encrypted_token = node.token_encrypted.as_ref()?;
    let decrypted_bytes = encryption_service.decrypt(encrypted_token).ok()?;
    let token = String::from_utf8(decrypted_bytes).ok()?;
    let deployer = temps_deployer::remote::RemoteNodeDeployer::new(
        node.address.clone(),
        token,
        node.name.clone(),
    )
    .ok()?;
    Some(Arc::new(deployer))
}

/// Drain a node: mark it as "draining" so no new replicas are scheduled on it,
/// and trigger redeployment of all affected environments so their containers
/// are rescheduled to healthy nodes.
#[utoipa::path(
    tag = "Nodes",
    post,
    path = "/internal/nodes/{node_id}/drain",
    params(
        ("node_id" = i32, Path, description = "Node ID")
    ),
    responses(
        (status = 200, description = "Node drain initiated", body = DrainNodeResponse),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Node not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
async fn admin_drain_node(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Path(node_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);
    let node = app_state
        .node_service
        .get_by_id(node_id)
        .await
        .map_err(Problem::from)?;

    if node.status == "draining" {
        return Err(problemdetails::new(StatusCode::CONFLICT)
            .with_title("Node Already Draining")
            .with_detail(format!("Node '{}' is already in draining state", node.name)));
    }

    // Get detailed info about each deployment on this node
    let affected = app_state
        .node_service
        .affected_deployments(node_id)
        .await
        .map_err(Problem::from)?;

    // Mark the node as draining — scheduler will exclude it from new assignments
    app_state
        .node_service
        .mark_draining(node_id)
        .await
        .map_err(Problem::from)?;

    let mut retired_count = 0usize;
    let mut redeployed_count = 0usize;

    for dep in &affected {
        if dep.needs_redeploy() {
            // All replicas are on this node — must redeploy to maintain availability
            match app_state
                .deployment_service
                .redeploy_environment(dep.project_id, dep.environment_id)
                .await
            {
                Ok(_) => {
                    redeployed_count += 1;
                    info!(
                        node_id,
                        project_id = dep.project_id,
                        environment_id = dep.environment_id,
                        "Drain: triggered full redeploy (no healthy replicas on other nodes)"
                    );
                }
                Err(e) => {
                    error!(
                        node_id,
                        project_id = dep.project_id,
                        environment_id = dep.environment_id,
                        "Drain: failed to trigger redeploy: {}",
                        e
                    );
                }
            }
        } else {
            // Other nodes still have healthy replicas — stop and retire containers on this node
            // First, stop containers on the agent (best-effort)
            let containers = app_state
                .node_service
                .list_containers_for_node_deployment(node_id, dep.deployment_id)
                .await
                .unwrap_or_default();

            if let Some(remote_deployer) =
                create_remote_deployer(&node, &app_state.encryption_service)
            {
                for container in &containers {
                    if let Err(e) = remote_deployer
                        .stop_container(&container.container_id)
                        .await
                    {
                        warn!(
                            node_id,
                            container_id = %container.container_id,
                            "Drain: failed to stop container on agent (will still retire): {}", e
                        );
                    }
                }
            }

            // Then soft-delete in DB so the proxy stops routing to them
            match app_state
                .node_service
                .retire_containers_on_node(node_id, dep.deployment_id)
                .await
            {
                Ok(count) => {
                    retired_count += count;
                    info!(
                        node_id,
                        deployment_id = dep.deployment_id,
                        retired = count,
                        remaining = dep.total_active_containers - dep.containers_on_node,
                        "Drain: retired containers, healthy replicas remain on other nodes"
                    );
                }
                Err(e) => {
                    error!(
                        node_id,
                        deployment_id = dep.deployment_id,
                        "Drain: failed to retire containers: {}",
                        e
                    );
                }
            }
        }
    }

    info!(
        node_id,
        node_name = %node.name,
        affected_deployments = affected.len(),
        retired_count,
        redeployed_count,
        "Node drain initiated"
    );

    let affected_count = affected.len();

    Ok(Json(DrainNodeResponse {
        id: node_id,
        name: node.name,
        status: "draining".to_string(),
        affected_environments: affected_count,
        message: format!(
            "Node drain initiated. {} deployment(s) affected: {} container(s) retired, {} environment(s) redeployed.",
            affected_count, retired_count, redeployed_count
        ),
    }))
}

/// Undrain (reactivate) a node so it can accept new deployments again.
/// Only works for nodes in "draining" or "drained" status.
#[utoipa::path(
    tag = "Nodes",
    delete,
    path = "/internal/nodes/{node_id}/drain",
    params(
        ("node_id" = i32, Path, description = "Node ID")
    ),
    responses(
        (status = 200, description = "Node reactivated", body = UndrainNodeResponse),
        (status = 400, description = "Node not in drainable state"),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Node not found"),
    ),
    security(("bearer_auth" = []))
)]
async fn admin_undrain_node(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Path(node_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);
    let node = app_state
        .node_service
        .get_by_id(node_id)
        .await
        .map_err(Problem::from)?;

    let node_name = node.name.clone();

    app_state
        .node_service
        .mark_active(node_id)
        .await
        .map_err(Problem::from)?;

    info!(node_id, node_name = %node_name, "Node undrained (reactivated)");

    Ok(Json(UndrainNodeResponse {
        id: node_id,
        name: node_name,
        status: "active".to_string(),
        message: "Node reactivated and ready to accept new deployments.".to_string(),
    }))
}

/// Remove a node from the cluster entirely. The node should be drained first
/// to ensure containers have been rescheduled. If the node still has active
/// containers, it will be drained automatically before removal.
#[utoipa::path(
    tag = "Nodes",
    delete,
    path = "/internal/nodes/{node_id}",
    params(
        ("node_id" = i32, Path, description = "Node ID")
    ),
    responses(
        (status = 200, description = "Node removed", body = RemoveNodeResponse),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Node not found"),
        (status = 409, description = "Node still has active containers"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
async fn admin_remove_node(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Path(node_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);
    let node = app_state
        .node_service
        .get_by_id(node_id)
        .await
        .map_err(Problem::from)?;

    // Check if node still has active containers
    let containers = app_state
        .node_service
        .list_containers_for_node(node_id)
        .await
        .map_err(Problem::from)?;

    if !containers.is_empty() {
        return Err(problemdetails::new(StatusCode::CONFLICT)
            .with_title("Node Has Active Containers")
            .with_detail(format!(
                "Node '{}' still has {} active container(s). Drain the node first with POST /internal/nodes/{}/drain",
                node.name, containers.len(), node_id
            )));
    }

    let node_name = node.name.clone();

    app_state
        .node_service
        .remove(node_id)
        .await
        .map_err(Problem::from)?;

    info!(node_id, node_name = %node_name, "Node removed from cluster");

    Ok(Json(RemoveNodeResponse {
        id: node_id,
        message: format!("Node '{}' removed from cluster", node_name),
    }))
}

/// Get the drain status for a node, including migration progress.
///
/// Returns container counts and whether the drain is complete.
/// Can be polled to track drain progress.
#[utoipa::path(
    tag = "Nodes",
    get,
    path = "/internal/nodes/{node_id}/drain",
    params(
        ("node_id" = i32, Path, description = "Node ID")
    ),
    responses(
        (status = 200, description = "Drain status", body = DrainStatusResponse),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Node not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
async fn admin_drain_status(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Path(node_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsRead);
    // Check drain completion first — this may transition the node to "drained"
    let _ = app_state
        .node_service
        .check_drain_complete(node_id)
        .await
        .map_err(Problem::from)?;

    // Re-fetch the node to get the potentially updated status
    let node = app_state
        .node_service
        .get_by_id(node_id)
        .await
        .map_err(Problem::from)?;

    let containers = app_state
        .node_service
        .list_containers_for_node(node_id)
        .await
        .map_err(Problem::from)?;

    let remaining = containers.len();
    let is_draining = node.status == "draining";
    let is_drained = node.status == "drained";
    let drain_complete = is_drained || (is_draining && remaining == 0);
    let can_remove = drain_complete || (node.status == "offline" && remaining == 0);

    let message = if is_drained || (is_draining && remaining == 0) {
        format!(
            "Drain complete. Node '{}' has no remaining containers and can be safely removed.",
            node.name
        )
    } else if is_draining {
        format!(
            "Draining: {} container(s) still on node '{}'. Workloads are being migrated.",
            remaining, node.name
        )
    } else {
        format!("Node '{}' is {} (not draining)", node.name, node.status)
    };

    Ok(Json(DrainStatusResponse {
        node_id,
        node_name: node.name,
        status: node.status,
        remaining_containers: remaining,
        drain_complete,
        can_remove,
        message,
    }))
}

// ---------------------------------------------------------------------------
// Edge analytics proxy — forwards queries from dashboard to edge nodes
// ---------------------------------------------------------------------------

/// Query params for edge analytics proxy endpoints.
#[derive(Deserialize, ToSchema)]
pub struct EdgeAnalyticsQuery {
    /// ISO 8601 start time
    pub since: Option<String>,
    /// ISO 8601 end time
    pub until: Option<String>,
    /// Max results
    pub limit: Option<u32>,
    /// Time bucket in minutes (for timeseries)
    pub bucket: Option<u32>,
    /// Filter by domain
    pub domain: Option<String>,
    /// Filter by edge node ID (omit to query all edge nodes)
    pub node_id: Option<i32>,
}

/// Edge node info for the dashboard.
#[derive(Serialize, ToSchema)]
pub struct EdgeNodeInfo {
    pub id: i32,
    pub name: String,
    pub status: String,
    pub region: Option<String>,
    pub api_address: Option<String>,
    pub last_heartbeat: Option<String>,
}

/// List all edge nodes.
async fn list_edge_nodes(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsRead);
    use sea_orm::{ColumnTrait, QueryFilter};
    use temps_entities::nodes;

    let edge_nodes = nodes::Entity::find()
        .filter(nodes::Column::Role.eq("edge"))
        .all(app_state.db.as_ref())
        .await
        .map_err(|e| {
            problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Database Error")
                .with_detail(format!("Failed to query edge nodes: {}", e))
        })?;

    let nodes: Vec<EdgeNodeInfo> = edge_nodes
        .into_iter()
        .map(|n| {
            let region = n
                .labels
                .get("region")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let api_address = n
                .labels
                .get("api_address")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            EdgeNodeInfo {
                id: n.id,
                name: n.name,
                status: n.status,
                region,
                api_address,
                last_heartbeat: n.last_heartbeat.map(|t| t.to_string()),
            }
        })
        .collect();

    Ok(Json(nodes))
}

/// Proxy an analytics query to edge node(s) and return the result.
/// If `node_id` is specified, queries that specific edge node.
/// Otherwise queries all active edge nodes and merges results.
async fn proxy_edge_query(
    app_state: &AppState,
    query: &EdgeAnalyticsQuery,
    endpoint: &str,
) -> Result<serde_json::Value, Problem> {
    use sea_orm::{ColumnTrait, QueryFilter};
    use temps_entities::nodes;

    // Find the target edge node(s)
    let mut finder = nodes::Entity::find().filter(nodes::Column::Role.eq("edge"));
    if let Some(nid) = query.node_id {
        finder = finder.filter(nodes::Column::Id.eq(nid));
    }
    // Only query active nodes
    finder = finder.filter(nodes::Column::Status.eq("active"));

    let edge_nodes = finder.all(app_state.db.as_ref()).await.map_err(|e| {
        problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
            .with_title("Database Error")
            .with_detail(format!("Failed to query edge nodes: {}", e))
    })?;

    if edge_nodes.is_empty() {
        return Ok(serde_json::json!([]));
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| {
            problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("HTTP Client Error")
                .with_detail(format!("{}", e))
        })?;

    let mut results = Vec::new();

    for node in &edge_nodes {
        let api_address = match node.labels.get("api_address").and_then(|v| v.as_str()) {
            Some(addr) => {
                // Replace 0.0.0.0 with 127.0.0.1 — 0.0.0.0 is a listen address, not routable
                addr.replace("0.0.0.0", "127.0.0.1")
            }
            None => {
                warn!(
                    "Edge node {} has no api_address in labels, skipping",
                    node.id
                );
                continue;
            }
        };

        // Validate api_address is a safe host:port — reject internal/metadata IPs (SSRF protection)
        if !is_safe_api_address(&api_address) {
            warn!(
                "Edge node {} has unsafe api_address '{}', skipping (SSRF protection)",
                node.id, api_address
            );
            continue;
        }

        // Decrypt the node's token to authenticate with its API
        let token = match &node.token_encrypted {
            Some(encrypted) => match app_state.encryption_service.decrypt_string(encrypted) {
                Ok(t) => t,
                Err(e) => {
                    warn!("Failed to decrypt token for edge node {}: {}", node.id, e);
                    continue;
                }
            },
            None => continue,
        };

        // Build query string
        let mut url = format!("http://{}/edge/analytics/{}", api_address, endpoint);
        let mut params = Vec::new();
        if let Some(ref since) = query.since {
            params.push(format!("since={}", urlencoding::encode(since)));
        }
        if let Some(ref until) = query.until {
            params.push(format!("until={}", urlencoding::encode(until)));
        }
        if let Some(limit) = query.limit {
            params.push(format!("limit={}", limit));
        }
        if let Some(bucket) = query.bucket {
            params.push(format!("bucket={}", bucket));
        }
        if let Some(ref domain) = query.domain {
            params.push(format!("domain={}", urlencoding::encode(domain)));
        }
        if !params.is_empty() {
            url = format!("{}?{}", url, params.join("&"));
        }

        match client
            .get(&url)
            .header("Authorization", format!("Bearer {}", token))
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                if let Ok(body) = resp.json::<serde_json::Value>().await {
                    let region = node
                        .labels
                        .get("region")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown");
                    results.push(serde_json::json!({
                        "node_id": node.id,
                        "node_name": node.name,
                        "region": region,
                        "data": body,
                    }));
                }
            }
            Ok(resp) => {
                warn!("Edge node {} returned {}", node.id, resp.status());
            }
            Err(e) => {
                warn!("Failed to query edge node {}: {}", node.id, e);
            }
        }
    }

    Ok(serde_json::Value::Array(results))
}

async fn proxy_edge_analytics_overview(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Query(query): Query<EdgeAnalyticsQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsRead);
    let results = proxy_edge_query(&app_state, &query, "overview").await?;
    Ok(Json(results))
}

async fn proxy_edge_analytics_domains(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Query(query): Query<EdgeAnalyticsQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsRead);
    let results = proxy_edge_query(&app_state, &query, "domains").await?;
    Ok(Json(results))
}

async fn proxy_edge_analytics_assets(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Query(query): Query<EdgeAnalyticsQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsRead);
    let results = proxy_edge_query(&app_state, &query, "assets").await?;
    Ok(Json(results))
}

async fn proxy_edge_analytics_timeseries(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Query(query): Query<EdgeAnalyticsQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsRead);
    let results = proxy_edge_query(&app_state, &query, "timeseries").await?;
    Ok(Json(results))
}

impl From<NodeError> for Problem {
    fn from(error: NodeError) -> Self {
        match error {
            NodeError::NotFound { ref name } => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Node Not Found")
                .with_detail(format!("Node '{}' not found", name)),
            NodeError::NotFoundById { node_id } => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Node Not Found")
                .with_detail(format!("Node with id {} not found", node_id)),
            NodeError::AlreadyExists { ref name } => problemdetails::new(StatusCode::CONFLICT)
                .with_title("Node Already Exists")
                .with_detail(format!("Node '{}' already exists", name)),
            NodeError::Validation { ref message } => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Validation Error")
                .with_detail(message.clone()),
            NodeError::Database(ref e) => {
                error!("Database error in node operation: {}", e);
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Internal Server Error")
                    .with_detail("An internal error occurred")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use temps_entities::{deployment_containers, nodes};
    use tower::ServiceExt;

    fn sample_node() -> nodes::Model {
        nodes::Model {
            id: 1,
            name: "worker-1".to_string(),
            token_hash: sha256_hash("test-token"),
            token_encrypted: None,
            address: "https://10.100.0.2:3100".to_string(),
            private_address: "10.100.0.2".to_string(),
            public_endpoint: None,
            wg_public_key: None,
            role: "worker".to_string(),
            status: "active".to_string(),
            labels: serde_json::json!({}),
            capacity: serde_json::json!({}),
            last_heartbeat: Some(chrono::Utc::now()),
            edge_public_key: None,
            compute_cidr: None,
            underlay_address: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    fn make_app(db: sea_orm::DatabaseConnection) -> axum::Router {
        make_app_with_settings(db, temps_core::AppSettings::default())
    }

    fn make_app_with_settings(
        db: sea_orm::DatabaseConnection,
        settings: temps_core::AppSettings,
    ) -> axum::Router {
        let db = Arc::new(db);
        // Create a separate mock DB for ConfigService that returns settings
        let settings_json = settings.to_json();
        let config_db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![temps_entities::settings::Model {
                id: 1,
                data: settings_json,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            }]])
            .into_connection();
        let server_config = Arc::new(temps_config::ServerConfig {
            address: "127.0.0.1:3000".to_string(),
            database_url: "postgres://test".to_string(),
            tls_address: None,
            console_address: "127.0.0.1:0".to_string(),
            data_dir: std::path::PathBuf::from("/tmp/temps-test"),
            auth_secret: "test-secret".to_string(),
            encryption_key: "test-key".to_string(),
            api_base_url: "/api".to_string(),
            postgres_max_connections: None,
            postgres_min_connections: None,
            postgres_connect_timeout_secs: None,
            postgres_acquire_timeout_secs: None,
            postgres_idle_timeout_secs: None,
            postgres_max_lifetime_secs: None,
            clickhouse_url: None,
            clickhouse_database: None,
            clickhouse_user: None,
            clickhouse_password: None,
        });
        let config_service = Arc::new(temps_config::ConfigService::new(
            server_config,
            Arc::new(config_db),
        ));
        let node_service = Arc::new(NodeService::new(db.clone()));
        let encryption_service = Arc::new(
            temps_core::EncryptionService::new("01234567890123456789012345678901").unwrap(),
        );
        let app_state = Arc::new(NodeAppState {
            node_service,
            db,
            config_service,
            encryption_service,
        });
        configure_routes().with_state(app_state)
    }

    fn settings_with_join_token() -> temps_core::AppSettings {
        let mut settings = temps_core::AppSettings::default();
        settings.multi_node.join_token_hash = Some(sha256_hash("test-join-token"));
        settings
    }

    #[tokio::test]
    async fn test_register_node_success() {
        let node = sample_node();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // Check for duplicate name (returns empty)
            .append_query_results(vec![Vec::<nodes::Model>::new()])
            // Insert returns the new node
            .append_query_results(vec![vec![node.clone()]])
            .into_connection();

        let app = make_app_with_settings(db, settings_with_join_token());
        let body = serde_json::json!({
            "name": "worker-1",
            "token": "test-token",
            "join_token": "test-join-token",
            "address": "https://10.100.0.2:3100",
            "private_address": "10.100.0.2"
        });

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/internal/nodes/register")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::CREATED);
    }

    #[tokio::test]
    async fn test_register_node_blocked_without_join_token_configured() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        // Default settings — no join token configured
        let app = make_app(db);

        let body = serde_json::json!({
            "name": "worker-1",
            "token": "test-token",
            "address": "https://10.100.0.2:3100",
            "private_address": "10.100.0.2"
        });

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/internal/nodes/register")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn test_register_node_empty_name_returns_400() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let app = make_app_with_settings(db, settings_with_join_token());

        let body = serde_json::json!({
            "name": "",
            "token": "test-token",
            "join_token": "test-join-token",
            "address": "https://10.100.0.2:3100",
            "private_address": "10.100.0.2"
        });

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/internal/nodes/register")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_heartbeat_missing_auth_returns_401() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let app = make_app(db);

        let body = serde_json::json!({ "capacity": {} });

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/internal/nodes/1/heartbeat")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_heartbeat_wrong_token_returns_401() {
        let node = sample_node();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // get_by_id returns the node
            .append_query_results(vec![vec![node]])
            .into_connection();

        let app = make_app(db);

        let body = serde_json::json!({ "capacity": {} });

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/internal/nodes/1/heartbeat")
                    .header("content-type", "application/json")
                    .header("authorization", "Bearer wrong-token")
                    .body(Body::from(serde_json::to_string(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn test_sha256_hash_deterministic() {
        let hash1 = sha256_hash("test-token");
        let hash2 = sha256_hash("test-token");
        assert_eq!(hash1, hash2);
        assert_eq!(hash1.len(), 64); // SHA-256 produces 64 hex chars
    }

    #[test]
    fn test_sha256_hash_different_inputs() {
        let hash1 = sha256_hash("token-a");
        let hash2 = sha256_hash("token-b");
        assert_ne!(hash1, hash2);
    }

    #[test]
    fn test_node_error_to_problem_not_found() {
        let problem: Problem = NodeError::NotFound {
            name: "worker-x".to_string(),
        }
        .into();
        assert_eq!(problem.status_code, StatusCode::NOT_FOUND);
    }

    #[test]
    fn test_node_error_to_problem_not_found_by_id() {
        let problem: Problem = NodeError::NotFoundById { node_id: 42 }.into();
        assert_eq!(problem.status_code, StatusCode::NOT_FOUND);
    }

    #[test]
    fn test_node_error_to_problem_already_exists() {
        let problem: Problem = NodeError::AlreadyExists {
            name: "worker-1".to_string(),
        }
        .into();
        assert_eq!(problem.status_code, StatusCode::CONFLICT);
    }

    #[test]
    fn test_node_error_to_problem_validation() {
        let problem: Problem = NodeError::Validation {
            message: "bad input".to_string(),
        }
        .into();
        assert_eq!(problem.status_code, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_register_node_with_valid_join_token_succeeds() {
        let node = sample_node();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<nodes::Model>::new()])
            .append_query_results(vec![vec![node.clone()]])
            .into_connection();

        let mut settings = temps_core::AppSettings::default();
        settings.multi_node.join_token_hash = Some(sha256_hash("valid-join-token"));

        let app = make_app_with_settings(db, settings);
        let body = serde_json::json!({
            "name": "worker-1",
            "token": "test-token",
            "join_token": "valid-join-token",
            "address": "https://10.100.0.2:3100",
            "private_address": "10.100.0.2"
        });

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/internal/nodes/register")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::CREATED);
    }

    #[tokio::test]
    async fn test_register_node_with_invalid_join_token_returns_403() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();

        let mut settings = temps_core::AppSettings::default();
        settings.multi_node.join_token_hash = Some(sha256_hash("correct-token"));

        let app = make_app_with_settings(db, settings);
        let body = serde_json::json!({
            "name": "worker-1",
            "token": "test-token",
            "join_token": "wrong-token",
            "address": "https://10.100.0.2:3100",
            "private_address": "10.100.0.2"
        });

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/internal/nodes/register")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn test_register_node_missing_join_token_when_required_returns_403() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();

        let mut settings = temps_core::AppSettings::default();
        settings.multi_node.join_token_hash = Some(sha256_hash("some-token"));

        let app = make_app_with_settings(db, settings);
        let body = serde_json::json!({
            "name": "worker-1",
            "token": "test-token",
            "address": "https://10.100.0.2:3100",
            "private_address": "10.100.0.2"
        });

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/internal/nodes/register")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    // Note: admin_list_nodes and admin_get_node use RequireAuth (session auth)
    // and are tested through the plugin system's auth middleware integration.
    // The agent-facing routes (register, heartbeat) are tested above with bearer tokens.

    // ── Heartbeat with container reconciliation ──────────────────

    #[tokio::test]
    async fn test_heartbeat_with_container_inventory_triggers_reconciliation() {
        // Setup: node is "offline", has 2 containers in DB, agent reports only 1
        let mut node = sample_node();
        node.status = "offline".to_string();
        node.token_hash = sha256_hash("test-token");

        let mut reactivated_node = node.clone();
        reactivated_node.status = "active".to_string();

        // Container tracked in DB: container-1 and container-2
        let c1 = deployment_containers::Model {
            id: 1,
            deployment_id: 10,
            container_id: "abc123def".to_string(),
            container_name: "app-1".to_string(),
            container_port: 8080,
            host_port: Some(30001),
            image_name: Some("myapp:latest".to_string()),
            status: Some("running".to_string()),
            service_name: None,
            created_at: chrono::Utc::now(),
            deployed_at: chrono::Utc::now(),
            ready_at: Some(chrono::Utc::now()),
            deleted_at: None,
            node_id: Some(1),
            exit_code: None,
            exit_reason: None,
            oom_killed: None,
            error_message: None,
            finished_at: None,
            started_at: None,
            cpu_limit_cores: None,
        };
        let c2 = deployment_containers::Model {
            id: 2,
            deployment_id: 11,
            container_id: "ghost456def".to_string(),
            container_name: "app-2".to_string(),
            container_port: 8080,
            host_port: Some(30002),
            image_name: Some("myapp:latest".to_string()),
            status: Some("running".to_string()),
            service_name: None,
            created_at: chrono::Utc::now(),
            deployed_at: chrono::Utc::now(),
            ready_at: Some(chrono::Utc::now()),
            deleted_at: None,
            node_id: Some(1),
            exit_code: None,
            exit_reason: None,
            oom_killed: None,
            error_message: None,
            finished_at: None,
            started_at: None,
            cpu_limit_cores: None,
        };
        let mut c2_updated = c2.clone();
        c2_updated.status = Some("removed".to_string());
        c2_updated.deleted_at = Some(chrono::Utc::now());

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // heartbeat: find_by_id (get node to verify token)
            .append_query_results(vec![vec![node.clone()]])
            // heartbeat: find_by_id again (inside heartbeat() method)
            .append_query_results(vec![vec![node.clone()]])
            // heartbeat: update (reactivate offline -> active)
            .append_query_results(vec![vec![reactivated_node]])
            // reconcile: list_containers_for_node
            .append_query_results(vec![vec![c1.clone(), c2.clone()]])
            // reconcile: update ghost container (c2) -> deleted
            .append_query_results(vec![vec![c2_updated]])
            .into_connection();

        let app = make_app(db);

        // Agent reports only container abc123def (ghost456def is missing)
        let body = serde_json::json!({
            "capacity": { "cpu_percent": 25.0 },
            "containers": [
                { "container_id": "abc123def", "container_name": "app-1" }
            ]
        });

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/internal/nodes/1/heartbeat")
                    .header("content-type", "application/json")
                    .header("authorization", "Bearer test-token")
                    .body(Body::from(serde_json::to_string(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_heartbeat_without_containers_skips_reconciliation() {
        // Normal heartbeat without container inventory — no reconciliation
        let mut node = sample_node();
        node.token_hash = sha256_hash("test-token");

        let updated_node = node.clone();

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // heartbeat: find_by_id (verify token)
            .append_query_results(vec![vec![node.clone()]])
            // heartbeat: find_by_id (inside heartbeat())
            .append_query_results(vec![vec![node]])
            // heartbeat: update
            .append_query_results(vec![vec![updated_node]])
            .into_connection();

        let app = make_app(db);
        let body = serde_json::json!({
            "capacity": { "cpu_percent": 50.0 }
        });

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/internal/nodes/1/heartbeat")
                    .header("content-type", "application/json")
                    .header("authorization", "Bearer test-token")
                    .body(Body::from(serde_json::to_string(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_heartbeat_with_empty_inventory_marks_all_stale() {
        // Agent reports zero containers — all DB containers should be marked deleted
        let mut node = sample_node();
        node.token_hash = sha256_hash("test-token");

        let updated_node = node.clone();

        let c1 = deployment_containers::Model {
            id: 1,
            deployment_id: 10,
            container_id: "orphan-1".to_string(),
            container_name: "app-1".to_string(),
            container_port: 8080,
            host_port: Some(30001),
            image_name: Some("myapp:latest".to_string()),
            status: Some("running".to_string()),
            service_name: None,
            created_at: chrono::Utc::now(),
            deployed_at: chrono::Utc::now(),
            ready_at: Some(chrono::Utc::now()),
            deleted_at: None,
            node_id: Some(1),
            exit_code: None,
            exit_reason: None,
            oom_killed: None,
            error_message: None,
            finished_at: None,
            started_at: None,
            cpu_limit_cores: None,
        };
        let mut c1_updated = c1.clone();
        c1_updated.status = Some("removed".to_string());
        c1_updated.deleted_at = Some(chrono::Utc::now());

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // heartbeat: find_by_id (verify token)
            .append_query_results(vec![vec![node.clone()]])
            // heartbeat: find_by_id (inside heartbeat())
            .append_query_results(vec![vec![node]])
            // heartbeat: update
            .append_query_results(vec![vec![updated_node]])
            // reconcile: list_containers_for_node
            .append_query_results(vec![vec![c1]])
            // reconcile: update orphan-1 -> deleted
            .append_query_results(vec![vec![c1_updated]])
            .into_connection();

        let app = make_app(db);
        let body = serde_json::json!({
            "capacity": { "cpu_percent": 10.0 },
            "containers": []
        });

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/internal/nodes/1/heartbeat")
                    .header("content-type", "application/json")
                    .header("authorization", "Bearer test-token")
                    .body(Body::from(serde_json::to_string(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    // ── Security fix tests ────────────────────────────────────

    #[test]
    fn test_constant_time_eq_equal_strings() {
        assert!(constant_time_eq(b"abcdef", b"abcdef"));
    }

    #[test]
    fn test_constant_time_eq_different_strings() {
        assert!(!constant_time_eq(b"abcdef", b"ghijkl"));
    }

    #[test]
    fn test_constant_time_eq_different_lengths() {
        assert!(!constant_time_eq(b"short", b"longer_string"));
    }

    #[test]
    fn test_constant_time_eq_empty() {
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn test_constant_time_eq_with_sha256_hashes() {
        let hash1 = sha256_hash("test-token");
        let hash2 = sha256_hash("test-token");
        let hash3 = sha256_hash("wrong-token");
        assert!(constant_time_eq(hash1.as_bytes(), hash2.as_bytes()));
        assert!(!constant_time_eq(hash1.as_bytes(), hash3.as_bytes()));
    }

    #[test]
    fn test_is_safe_api_address_valid() {
        assert!(is_safe_api_address("10.0.0.5:3200"));
        assert!(is_safe_api_address("192.168.1.100:8080"));
        assert!(is_safe_api_address("edge-node.example.com:3200"));
    }

    #[test]
    fn test_is_safe_api_address_blocks_metadata() {
        // AWS/GCP/Azure metadata endpoint
        assert!(!is_safe_api_address("169.254.169.254:80"));
        // Link-local range
        assert!(!is_safe_api_address("169.254.1.1:80"));
    }

    #[test]
    fn test_is_safe_api_address_blocks_loopback() {
        assert!(!is_safe_api_address("127.0.0.1:3200"));
        assert!(!is_safe_api_address("127.0.0.2:8080"));
    }

    #[test]
    fn test_is_safe_api_address_blocks_injection() {
        // Path injection attempt
        assert!(!is_safe_api_address("evil.com/admin#:3200"));
        // @ injection
        assert!(!is_safe_api_address("user@internal:3200"));
    }

    #[test]
    fn test_is_safe_api_address_rejects_missing_port() {
        assert!(!is_safe_api_address("10.0.0.5"));
        assert!(!is_safe_api_address("example.com"));
    }

    #[test]
    fn test_is_safe_api_address_rejects_invalid_port() {
        assert!(!is_safe_api_address("10.0.0.5:notaport"));
    }

    #[test]
    fn test_is_safe_api_address_blocks_unspecified() {
        assert!(!is_safe_api_address("0.0.0.0:3200"));
    }
}
