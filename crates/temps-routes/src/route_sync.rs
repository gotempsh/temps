//! Internal route-sync endpoint — per-node agents long-poll this for the
//! `*.temps.local` routing table their internal edge proxy will serve.
//!
//! ## Wire model
//!
//! - `GET /internal/nodes/{node_id}/routes/snapshot?since=N` — long-poll.
//!   Returns a full `RouteSnapshot` whenever the CP's in-memory route
//!   generation moves past `N`. Times out after 25 s with the current
//!   snapshot so a worker that lost wakeups still converges.
//! - `POST /internal/nodes/{node_id}/routes/ack` — agent reports the
//!   highest applied generation. Currently informational; useful later
//!   for ops drift detection (mirror of `node_dns_state`).
//!
//! ## Why a full snapshot every time
//!
//! Internal-zone routes are tiny (~one row per active deployment). A
//! deltas-and-tombstones protocol would save bytes but cost a more
//! complex apply path on the agent. We optimise for *correctness under
//! restart and reconnect*: the agent can hydrate from a single snapshot
//! and never has to reconcile partial state. CP restart resets the
//! generation counter; agents detect (`current < applied`) and re-fetch.
//!
//! ## Auth
//!
//! Same scheme as `temps-dns::handlers::dns_sync`: per-node bearer
//! token, sha256-compared in constant time against `nodes.token_hash`.
//! Path's `node_id` must match the token's node — a worker cannot fetch
//! another worker's view (today the snapshot is identical for every
//! worker; we still gate by node so a future per-node-filtered view is
//! a non-breaking refinement).

use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use sea_orm::sea_query::OnConflict;
use sea_orm::{ActiveValue::Set, DatabaseConnection, EntityTrait};
use serde::{Deserialize, Serialize};
use temps_entities::{node_route_state, nodes};
use tracing::{error, warn};

use crate::route_table::{BackendType, CachedPeerTable, RouteInfo};

/// Hold the long-poll request open this long before returning the
/// current snapshot when no generation bump arrives. Slightly less
/// than typical idle-timeout boundaries on intermediate proxies.
const LONG_POLL_TIMEOUT: Duration = Duration::from_secs(25);

pub struct RouteSyncAppState {
    pub db: Arc<DatabaseConnection>,
    pub peer_table: Arc<CachedPeerTable>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RouteSnapshotQuery {
    /// Highest generation the agent has already applied. Pass `0`
    /// for first-time-fetch. The handler returns immediately whenever
    /// `current_generation > since`; otherwise it sleeps until the
    /// next reload or the long-poll timeout.
    #[serde(default)]
    pub since: u64,
}

/// One backend instance behind a host. `address` is "ip:port" exactly
/// the way the proxy will dial it (overlay IP for local-node
/// containers, underlay-IP+published-port for cross-node).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteBackendDto {
    pub address: String,
    pub container_id: Option<String>,
    pub container_name: Option<String>,
}

/// One internal-zone route. Workers index by `host` (lower-cased) and
/// pick from `backends`. `deployment_id` is sent back through the
/// proxy chain as a header for log correlation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteEntryDto {
    pub host: String,
    pub backends: Vec<RouteBackendDto>,
    pub deployment_id: Option<i32>,
    pub project_id: Option<i32>,
    pub environment_id: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteSnapshot {
    pub generation: u64,
    pub routes: Vec<RouteEntryDto>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RouteAckRequest {
    pub applied_generation: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct RouteAckResponse {
    pub node_id: i32,
    pub applied_generation: u64,
    pub server_generation: u64,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

pub async fn get_routes_snapshot(
    State(app_state): State<Arc<RouteSyncAppState>>,
    headers: HeaderMap,
    Path(node_id): Path<i32>,
    Query(q): Query<RouteSnapshotQuery>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    authenticate_node(&app_state.db, &headers, node_id).await?;

    // Fast path: generation already moved past `since` — return now.
    let mut current = app_state.peer_table.current_generation();
    if current > q.since {
        return Ok(Json(build_snapshot(&app_state.peer_table, current)));
    }

    // Slow path: park on the notifier until either a reload happens
    // or the long-poll deadline fires. We don't block forever — many
    // intermediate proxies (and the agent's HTTP client) drop idle
    // connections after ~30 s, so we return a same-generation snapshot
    // before that point and let the agent reconnect cleanly.
    let notifier = app_state.peer_table.generation_notifier();
    let _ = tokio::time::timeout(LONG_POLL_TIMEOUT, async {
        // Loop guards against spurious wakeups: keep waiting until the
        // generation actually moves.
        loop {
            let notified = notifier.notified();
            tokio::pin!(notified);
            notified.await;
            if app_state.peer_table.current_generation() > q.since {
                break;
            }
        }
    })
    .await;
    current = app_state.peer_table.current_generation();
    Ok(Json(build_snapshot(&app_state.peer_table, current)))
}

pub async fn post_routes_ack(
    State(app_state): State<Arc<RouteSyncAppState>>,
    headers: HeaderMap,
    Path(node_id): Path<i32>,
    Json(body): Json<RouteAckRequest>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    authenticate_node(&app_state.db, &headers, node_id).await?;
    let server_generation = app_state.peer_table.current_generation();

    // Persist the ACK so `mark_deployment_complete` can wait until
    // every healthy worker has applied the new route generation
    // before the deployment is declared "completed". This is the
    // worker-side half of the route-propagation barrier; the CP-side
    // half is the existing `RouteTableUpdated` event the listener
    // emits after `load_routes()`.
    //
    // We use applied_generation as a signed bigint to match the
    // existing column type in node_dns_state. Workers ack u64s but
    // route_generation in practice fits comfortably in i64 (we'd
    // need 9.2e18 reloads to overflow).
    let applied_i64: i64 = body.applied_generation.try_into().unwrap_or(i64::MAX);
    let now = chrono::Utc::now();
    let upsert = node_route_state::ActiveModel {
        node_id: Set(node_id),
        applied_generation: Set(applied_i64),
        last_sync_at: Set(Some(now)),
        health: Set("healthy".to_string()),
    };
    if let Err(e) = node_route_state::Entity::insert(upsert)
        .on_conflict(
            OnConflict::column(node_route_state::Column::NodeId)
                .update_columns([
                    node_route_state::Column::AppliedGeneration,
                    node_route_state::Column::LastSyncAt,
                    node_route_state::Column::Health,
                ])
                .to_owned(),
        )
        .exec(app_state.db.as_ref())
        .await
    {
        // Logging only — return success so the agent's sync loop
        // doesn't back off. ACK persistence is best-effort; the
        // worker still has the snapshot in memory and on disk.
        warn!(node_id, error = %e, "failed to persist route ack");
    }

    Ok(Json(RouteAckResponse {
        node_id,
        applied_generation: body.applied_generation,
        server_generation,
    }))
}

pub fn configure_routes() -> axum::Router<Arc<RouteSyncAppState>> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route(
            "/internal/nodes/{node_id}/routes/snapshot",
            get(get_routes_snapshot),
        )
        .route(
            "/internal/nodes/{node_id}/routes/ack",
            post(post_routes_ack),
        )
}

// ---------------------------------------------------------------------------
// Snapshot builder
// ---------------------------------------------------------------------------

fn build_snapshot(peer_table: &CachedPeerTable, generation: u64) -> RouteSnapshot {
    let raw = peer_table.snapshot_internal_routes();
    let mut routes = Vec::with_capacity(raw.len());
    for (host, info) in raw {
        if let Some(entry) = entry_from_route(host, &info) {
            routes.push(entry);
        }
    }
    RouteSnapshot { generation, routes }
}

fn entry_from_route(host: String, info: &RouteInfo) -> Option<RouteEntryDto> {
    // Internal zone is always proxied to live containers; static-dir
    // routes don't make sense here and are skipped.
    let backends = match &info.backend {
        BackendType::Upstream { backends, .. } => backends
            .iter()
            .map(|b| RouteBackendDto {
                address: b.address.clone(),
                container_id: b.container_id.clone(),
                container_name: b.container_name.clone(),
            })
            .collect(),
        BackendType::StaticDir { .. } => return None,
    };
    Some(RouteEntryDto {
        host,
        backends,
        deployment_id: info.deployment.as_ref().map(|d| d.id),
        project_id: info.project.as_ref().map(|p| p.id),
        environment_id: info.environment.as_ref().map(|e| e.id),
    })
}

// ---------------------------------------------------------------------------
// Auth helper (same shape as temps-dns dns_sync)
// ---------------------------------------------------------------------------

async fn authenticate_node(
    db: &DatabaseConnection,
    headers: &HeaderMap,
    node_id: i32,
) -> Result<(), (StatusCode, String)> {
    let token = extract_bearer_token(headers)?;
    let node = nodes::Entity::find_by_id(node_id)
        .one(db)
        .await
        .map_err(|e| {
            error!(node_id, "node lookup failed: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to look up node {}: {}", node_id, e),
            )
        })?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                format!("Node {} does not exist", node_id),
            )
        })?;
    let token_hash = sha256_hash(&token);
    if !constant_time_eq(node.token_hash.as_bytes(), token_hash.as_bytes()) {
        warn!(node_id, "Invalid route sync token");
        return Err((
            StatusCode::UNAUTHORIZED,
            format!("Invalid authentication token for node {}", node_id),
        ));
    }
    Ok(())
}

fn extract_bearer_token(headers: &HeaderMap) -> Result<String, (StatusCode, String)> {
    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .ok_or((
            StatusCode::UNAUTHORIZED,
            "Bearer token required".to_string(),
        ))?;
    let token = auth.strip_prefix("Bearer ").ok_or((
        StatusCode::UNAUTHORIZED,
        "Authorization header must use Bearer scheme".to_string(),
    ))?;
    Ok(token.to_string())
}

fn sha256_hash(input: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    format!("{:x}", hasher.finalize())
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
