// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The node state stream's HTTP surface (ADR-055 §7).
//!
//! Node-authenticated (the node's bearer token, as on the heartbeat):
//! - `GET /internal/nodes/{node_id}/state?since=W`: one long-poll read;
//! - `POST /internal/nodes/{node_id}/state/ack`: the cursors the node
//!   applied, per object kind;
//! - `GET /internal/nodes/{node_id}/instances/{instance_id}/env`: the
//!   environment of one of the node's instances, at create time. The only
//!   response carrying secret values: `Cache-Control: no-store`, never
//!   logged, and audited (which node received which instance's environment,
//!   with the variable count, never a value).
//!
//! Session-authenticated: `GET /node-state/status`, the stream's metrics.
//!
//! Acks are cursor bookkeeping a node sends after every read, like its
//! heartbeat; they are not audited.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    extract::{ConnectInfo, Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use temps_auth::{permission_guard, RequireAuth};
use temps_core::problemdetails::{self, Problem};
use temps_core::{AuditContext, AuditLogger, AuditOperation};
use temps_workload::stream::StateAck;
use tracing::{error, warn};
use utoipa::{OpenApi, ToSchema};

use crate::handlers::nodes::{constant_time_eq, extract_bearer_token, sha256_hash};
use crate::services::{NodeService, NodeStateError, NodeStateService};

pub struct NodeStateAppState {
    pub node_service: Arc<NodeService>,
    pub node_state: Arc<NodeStateService>,
    pub audit_service: Arc<dyn AuditLogger>,
}

/// The session-authenticated status route's state.
pub struct NodeStateStatusAppState {
    pub node_state: Arc<NodeStateService>,
}

impl From<NodeStateError> for Problem {
    fn from(error: NodeStateError) -> Self {
        match &error {
            NodeStateError::PlaneNotRunning { .. } => {
                problemdetails::new(StatusCode::SERVICE_UNAVAILABLE)
                    .with_title("Workload Plane Not Running")
                    .with_detail(error.to_string())
            }
            NodeStateError::InvalidCursor { .. } => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Invalid Stream Cursor")
                .with_detail(error.to_string()),
            NodeStateError::InstanceNotOnNode { .. } => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Instance Not On Node")
                .with_detail(error.to_string()),
            NodeStateError::Plane { .. } => {
                error!("node state stream: {error}");
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Node State Stream Error")
                    .with_detail(error.to_string())
            }
        }
    }
}

#[derive(Debug, Default, Deserialize, utoipa::IntoParams)]
pub struct StateQuery {
    /// The cursor of the last answer applied (`0` or absent: the whole view).
    pub since: Option<String>,
    /// How long to wait for a change, in seconds (at most 25).
    pub wait_secs: Option<u64>,
}

/// The stream's state and metrics.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct NodeStateStatusResponse {
    /// Whether the workload plane (and so the stream) runs in this process.
    pub configured: bool,
    /// Why not, when `configured` is false.
    pub reason: Option<String>,
    /// How far the visibility watermark lags behind the oldest marker it
    /// holds back, in milliseconds (an old open transaction holds it).
    pub watermark_lag_ms: u64,
    pub reads: u64,
    pub full_snapshots: u64,
    pub markers_swept: u64,
}

#[derive(Debug, Clone, Serialize)]
struct NodeInstanceEnvironmentDeliveredAudit {
    context: AuditContext,
    node_id: i32,
    instance_id: i64,
    variable_count: usize,
}

impl AuditOperation for NodeInstanceEnvironmentDeliveredAudit {
    fn operation_type(&self) -> String {
        "NODE_INSTANCE_ENVIRONMENT_DELIVERED".to_string()
    }
    fn user_id(&self) -> Option<i32> {
        // A node with its own token, not a user.
        None
    }
    fn ip_address(&self) -> Option<String> {
        self.context.ip_address.clone()
    }
    fn user_agent(&self) -> &str {
        &self.context.user_agent
    }
    fn serialize(&self) -> anyhow::Result<String> {
        serde_json::to_string(self)
            .map_err(|e| anyhow::anyhow!("Failed to serialize audit operation {}", e))
    }
}

/// Verify `headers` carry node `node_id`'s token.
async fn authenticate_node(
    state: &NodeStateAppState,
    headers: &HeaderMap,
    node_id: i32,
) -> Result<(), Problem> {
    let token = extract_bearer_token(headers)?;
    let node = state
        .node_service
        .get_by_id(node_id)
        .await
        .map_err(Problem::from)?;
    if !constant_time_eq(node.token_hash.as_bytes(), sha256_hash(&token).as_bytes()) {
        warn!(node_id, "Invalid node token on the state stream");
        return Err(problemdetails::new(StatusCode::UNAUTHORIZED)
            .with_title("Invalid Token")
            .with_detail(format!("Invalid authentication token for node {node_id}")));
    }
    Ok(())
}

pub fn configure_routes() -> Router<Arc<NodeStateAppState>> {
    Router::new()
        .route("/internal/nodes/{node_id}/state", get(read_node_state))
        .route("/internal/nodes/{node_id}/state/ack", post(ack_node_state))
        .route(
            "/internal/nodes/{node_id}/instances/{instance_id}/env",
            get(node_instance_env),
        )
}

pub fn configure_status_routes() -> Router<Arc<NodeStateStatusAppState>> {
    Router::new().route("/node-state/status", get(node_state_status))
}

/// Read a node's desired state since a cursor (long-poll)
#[utoipa::path(
    tag = "Nodes",
    get,
    path = "/internal/nodes/{node_id}/state",
    params(
        ("node_id" = i32, Path, description = "Node ID"),
        StateQuery
    ),
    responses(
        (status = 200, description = "Changes since the cursor, or the whole view", body = Object),
        (status = 400, description = "Invalid cursor"),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Node not found"),
        (status = 503, description = "The workload plane is not running"),
        (status = 500, description = "Internal server error")
    )
)]
async fn read_node_state(
    State(state): State<Arc<NodeStateAppState>>,
    headers: HeaderMap,
    Path(node_id): Path<i32>,
    Query(query): Query<StateQuery>,
) -> Result<impl IntoResponse, Problem> {
    authenticate_node(&state, &headers, node_id).await?;
    let wait = Duration::from_secs(query.wait_secs.unwrap_or(0));
    let answer = state
        .node_state
        .read(node_id, query.since.as_deref(), wait)
        .await?;
    Ok(Json(answer))
}

/// Record the cursors a node applied, per object kind
#[utoipa::path(
    tag = "Nodes",
    post,
    path = "/internal/nodes/{node_id}/state/ack",
    params(("node_id" = i32, Path, description = "Node ID")),
    request_body = Object,
    responses(
        (status = 204, description = "Recorded"),
        (status = 400, description = "Invalid cursor"),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Node not found"),
        (status = 503, description = "The workload plane is not running"),
        (status = 500, description = "Internal server error")
    )
)]
async fn ack_node_state(
    State(state): State<Arc<NodeStateAppState>>,
    headers: HeaderMap,
    Path(node_id): Path<i32>,
    Json(ack): Json<StateAck>,
) -> Result<impl IntoResponse, Problem> {
    authenticate_node(&state, &headers, node_id).await?;
    state.node_state.ack(node_id, &ack).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// The environment of one of a node's instances, to create it
#[utoipa::path(
    tag = "Nodes",
    get,
    path = "/internal/nodes/{node_id}/instances/{instance_id}/env",
    params(
        ("node_id" = i32, Path, description = "Node ID"),
        ("instance_id" = i64, Path, description = "Workload instance ID")
    ),
    responses(
        (status = 200, description = "The instance's environment (not cacheable)", body = Object),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Not an unfinished instance of this node"),
        (status = 503, description = "The workload plane is not running"),
        (status = 500, description = "Internal server error")
    )
)]
async fn node_instance_env(
    State(state): State<Arc<NodeStateAppState>>,
    headers: HeaderMap,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Path((node_id, instance_id)): Path<(i32, i64)>,
) -> Result<impl IntoResponse, Problem> {
    authenticate_node(&state, &headers, node_id).await?;
    let env = state.node_state.instance_env(node_id, instance_id).await?;
    let audit = NodeInstanceEnvironmentDeliveredAudit {
        context: AuditContext {
            user_id: 0,
            ip_address: Some(addr.ip().to_string()),
            user_agent: format!("temps-agent/node-{node_id}"),
        },
        node_id,
        instance_id,
        variable_count: env.env.len(),
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log: {}", e);
    }
    Ok((
        [(header::CACHE_CONTROL, HeaderValue::from_static("no-store"))],
        Json(env),
    ))
}

/// The node state stream's status and metrics
#[utoipa::path(
    tag = "Nodes",
    get,
    path = "/node-state/status",
    responses(
        (status = 200, description = "Stream status", body = NodeStateStatusResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions")
    ),
    security(("bearer_auth" = []))
)]
async fn node_state_status(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<NodeStateStatusAppState>>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsRead);
    let status = state.node_state.status();
    let metrics = status
        .metrics
        .unwrap_or(temps_workload_plane::StreamMetricsSnapshot {
            watermark_lag_ms: 0,
            reads: 0,
            full_snapshots: 0,
            markers_swept: 0,
        });
    Ok(Json(NodeStateStatusResponse {
        configured: status.running,
        reason: status.reason,
        watermark_lag_ms: metrics.watermark_lag_ms,
        reads: metrics.reads,
        full_snapshots: metrics.full_snapshots,
        markers_swept: metrics.markers_swept,
    }))
}

#[derive(OpenApi)]
#[openapi(
    paths(read_node_state, ack_node_state, node_instance_env, node_state_status),
    components(schemas(NodeStateStatusResponse))
)]
pub struct NodeStateApiDoc;

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::extract::connect_info::MockConnectInfo;
    use axum::http::Request;
    use tower::ServiceExt;

    #[derive(Default)]
    struct NoAudit;

    #[async_trait::async_trait]
    impl AuditLogger for NoAudit {
        async fn create_audit_log(&self, _: &dyn AuditOperation) -> anyhow::Result<()> {
            Ok(())
        }
    }

    async fn router() -> Option<(Router, temps_database::test_utils::TestDatabase)> {
        let db = match temps_database::test_utils::TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(e) => {
                println!("Database not available, skipping: {e}");
                return None;
            }
        };
        let state = Arc::new(NodeStateAppState {
            node_service: Arc::new(NodeService::new(db.db.clone())),
            node_state: Arc::new(NodeStateService::new()),
            audit_service: Arc::new(NoAudit),
        });
        let addr: SocketAddr = "10.233.0.20:4000".parse().expect("addr");
        Some((
            configure_routes()
                .with_state(state)
                .layer(MockConnectInfo(addr)),
            db,
        ))
    }

    #[tokio::test]
    async fn the_stream_refuses_a_request_without_a_node_token() {
        let Some((router, _db)) = router().await else {
            return;
        };
        let response = router
            .oneshot(
                Request::get("/internal/nodes/1/state?since=0")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn the_stream_of_an_unknown_node_is_not_found() {
        let Some((router, _db)) = router().await else {
            return;
        };
        let response = router
            .oneshot(
                Request::get("/internal/nodes/4242/instances/1/env")
                    .header("authorization", "Bearer some-token")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn errors_map_to_their_statuses() {
        let status = |error: NodeStateError| Problem::from(error).status_code;
        assert_eq!(
            status(NodeStateError::PlaneNotRunning {
                node_id: 2,
                operation: "read its state stream"
            }),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            status(NodeStateError::InvalidCursor {
                node_id: 2,
                reason: "cursor 'x' is not a transaction id watermark".into()
            }),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            status(NodeStateError::InstanceNotOnNode {
                node_id: 2,
                instance_id: 9
            }),
            StatusCode::NOT_FOUND
        );
    }
}
