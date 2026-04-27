//! Internal DNS sync endpoint — per-node Hickory resolvers poll this for
//! the records they should serve in the `*.temps.local` zone (ADR-011).
//!
//! ## Auth
//!
//! Same scheme as `temps-deployments::handlers::network`: per-node bearer
//! token presented in `Authorization: Bearer …`, sha256-hashed and compared
//! in constant time against `nodes.token_hash`. The `node_id` in the path
//! must match the token's node — a worker cannot fetch another worker's
//! state.
//!
//! ## Two endpoints
//!
//! - `GET /internal/nodes/{node_id}/dns/changes?since=N` — long-poll style
//!   diff. Returns records with `generation > N`. If `N == 0` (or the diff
//!   exceeds an internal threshold), returns `full_snapshot: true` + the
//!   entire zone.
//! - `POST /internal/nodes/{node_id}/dns/ack` — agent reports the highest
//!   generation it has applied. Updates `node_dns_state` so ops can detect
//!   drift.
//!
//! Both endpoints are deliberately *not* under `RequireAuth` (which expects
//! user JWT). They use raw bearer-token validation against the nodes table.

use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use sea_orm::{DatabaseConnection, EntityTrait};
use serde::{Deserialize, Serialize};
use temps_core::problemdetails::{self, Problem};
use temps_entities::{nodes, service_endpoints};
use tracing::{error, warn};
use utoipa::{IntoParams, ToSchema};

use crate::services::{DnsRegistry, DnsRegistryError};

/// Application state for the internal DNS sync endpoints.
///
/// Distinct from `DnsAppState` (the user-facing DNS provider state) because
/// the sync endpoints have a different auth model and a different consumer
/// (the per-node agent, not interactive users). Wiring up both states in
/// the plugin means the user-facing handlers stay free of `DnsRegistry`
/// noise and vice versa.
pub struct DnsSyncAppState {
    pub registry: Arc<DnsRegistry>,
    pub db: Arc<DatabaseConnection>,
}

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, IntoParams)]
pub struct DnsChangesQuery {
    /// Highest generation the agent has already applied. Pass `0` to
    /// request a full zone snapshot. Defaults to `0` if omitted.
    #[serde(default)]
    pub since: i64,
}

/// One DNS record on the wire. Mirrors `service_endpoints::Model` but
/// keeps the API stable across entity evolution. `target_ip` is a string
/// (v4 or v6 literal, or CNAME target hostname) parsed by the resolver.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct EndpointDto {
    pub id: i64,
    pub fqdn: String,
    pub record_type: String,
    pub target_ip: Option<String>,
    pub target_port: Option<i32>,
    pub ttl: i32,
    pub owner_kind: String,
    pub owner_id: i64,
    pub node_id: Option<i32>,
    pub generation: i64,
}

impl From<service_endpoints::Model> for EndpointDto {
    fn from(m: service_endpoints::Model) -> Self {
        Self {
            id: m.id,
            fqdn: m.fqdn,
            record_type: m.record_type,
            target_ip: m.target_ip,
            target_port: m.target_port,
            ttl: m.ttl,
            owner_kind: m.owner_kind,
            owner_id: m.owner_id,
            node_id: m.node_id,
            generation: m.generation,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DnsChangesResponse {
    /// Highest generation included in this response. Agent ACKs this back.
    pub generation: i64,
    /// `true` ⇒ replace the local zone with `records`. `false` ⇒ merge
    /// `records` into the existing zone (and remove `removed_ids`).
    pub full_snapshot: bool,
    pub records: Vec<EndpointDto>,
    /// IDs the agent should remove from its zone. Always empty in the v1
    /// protocol — the resolver reconciles by name on snapshot mode. Kept
    /// in the wire format so a future tombstone-based protocol doesn't
    /// require a breaking change.
    pub removed_ids: Vec<i64>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct DnsAckRequest {
    /// Highest generation the agent has actually applied locally.
    pub applied_generation: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DnsAckResponse {
    pub node_id: i32,
    pub applied_generation: i64,
    pub server_generation: i64,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// `GET /internal/nodes/{node_id}/dns/changes?since=N`
#[utoipa::path(
    tag = "Internal DNS",
    get,
    path = "/internal/nodes/{node_id}/dns/changes",
    params(
        ("node_id" = i32, Path, description = "Node id, must match the bearer token's node"),
        DnsChangesQuery,
    ),
    responses(
        (status = 200, description = "Diff or full snapshot", body = DnsChangesResponse),
        (status = 401, description = "Missing or invalid bearer token"),
        (status = 404, description = "Node not found"),
        (status = 500, description = "Internal server error")
    )
)]
pub async fn get_dns_changes(
    State(app_state): State<Arc<DnsSyncAppState>>,
    headers: HeaderMap,
    Path(node_id): Path<i32>,
    Query(q): Query<DnsChangesQuery>,
) -> Result<impl IntoResponse, Problem> {
    authenticate_node(&app_state.db, &headers, node_id).await?;

    let change_set = app_state
        .registry
        .get_changes_since(q.since)
        .await
        .map_err(Problem::from)?;

    Ok(Json(DnsChangesResponse {
        generation: change_set.generation,
        full_snapshot: change_set.full_snapshot,
        records: change_set
            .records
            .into_iter()
            .map(EndpointDto::from)
            .collect(),
        removed_ids: change_set.removed_ids,
    }))
}

/// `POST /internal/nodes/{node_id}/dns/ack`
#[utoipa::path(
    tag = "Internal DNS",
    post,
    path = "/internal/nodes/{node_id}/dns/ack",
    params(
        ("node_id" = i32, Path, description = "Node id, must match the bearer token's node"),
    ),
    request_body = DnsAckRequest,
    responses(
        (status = 200, description = "ACK accepted", body = DnsAckResponse),
        (status = 400, description = "ACK higher than server generation"),
        (status = 401, description = "Missing or invalid bearer token"),
        (status = 404, description = "Node not found"),
        (status = 500, description = "Internal server error")
    )
)]
pub async fn post_dns_ack(
    State(app_state): State<Arc<DnsSyncAppState>>,
    headers: HeaderMap,
    Path(node_id): Path<i32>,
    Json(body): Json<DnsAckRequest>,
) -> Result<impl IntoResponse, Problem> {
    authenticate_node(&app_state.db, &headers, node_id).await?;

    let state = app_state
        .registry
        .ack_applied(node_id, body.applied_generation)
        .await
        .map_err(Problem::from)?;

    // Read current generation back via a lightweight changes-since-current
    // call — gives the agent a hint about whether more work is pending.
    let server_generation = app_state
        .registry
        .get_changes_since(state.applied_generation)
        .await
        .map(|c| c.generation)
        .unwrap_or(state.applied_generation);

    Ok(Json(DnsAckResponse {
        node_id: state.node_id,
        applied_generation: state.applied_generation,
        server_generation,
    }))
}

// ---------------------------------------------------------------------------
// Auth helper
// ---------------------------------------------------------------------------
//
// Mirrors `temps-deployments::handlers::network::list_peers`. We deliberately
// duplicate rather than depend on `temps-deployments` (which would invert the
// crate dependency graph). The helper is small enough that the duplication
// cost is lower than the dep-graph cost.

async fn authenticate_node(
    db: &DatabaseConnection,
    headers: &HeaderMap,
    node_id: i32,
) -> Result<(), Problem> {
    let token = extract_bearer_token(headers)?;
    let node = nodes::Entity::find_by_id(node_id)
        .one(db)
        .await
        .map_err(|e| {
            error!(node_id, "node lookup failed: {}", e);
            problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Database Error")
                .with_detail(format!("Failed to look up node {}: {}", node_id, e))
        })?
        .ok_or_else(|| {
            problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Node Not Found")
                .with_detail(format!("Node {} does not exist", node_id))
        })?;
    let token_hash = sha256_hash(&token);
    if !constant_time_eq(node.token_hash.as_bytes(), token_hash.as_bytes()) {
        warn!(node_id, "Invalid DNS sync token");
        return Err(problemdetails::new(StatusCode::UNAUTHORIZED)
            .with_title("Invalid Token")
            .with_detail(format!("Invalid authentication token for node {}", node_id)));
    }
    Ok(())
}

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

// ---------------------------------------------------------------------------
// DnsRegistryError -> Problem
// ---------------------------------------------------------------------------

impl From<DnsRegistryError> for Problem {
    fn from(error: DnsRegistryError) -> Self {
        match error {
            DnsRegistryError::NotFound { .. } => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("DNS Endpoint Not Found")
                .with_detail(error.to_string()),
            DnsRegistryError::NodeStateNotFound { .. } => {
                problemdetails::new(StatusCode::NOT_FOUND)
                    .with_title("Node DNS State Not Found")
                    .with_detail(error.to_string())
            }
            DnsRegistryError::Validation { .. } | DnsRegistryError::InvalidIp { .. } => {
                problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Validation Error")
                    .with_detail(error.to_string())
            }
            DnsRegistryError::AckTooHigh { .. } => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("ACK Generation Out Of Range")
                .with_detail(error.to_string()),
            DnsRegistryError::Database(_) => problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Internal Server Error")
                .with_detail(error.to_string()),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests (handler-local — service-layer behaviour is tested in the registry).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn extract_bearer_token_happy() {
        let mut h = HeaderMap::new();
        h.insert("authorization", HeaderValue::from_static("Bearer abc123"));
        assert_eq!(extract_bearer_token(&h).unwrap(), "abc123");
    }

    #[test]
    fn extract_bearer_token_missing() {
        let h = HeaderMap::new();
        assert!(extract_bearer_token(&h).is_err());
    }

    #[test]
    fn extract_bearer_token_wrong_scheme() {
        let mut h = HeaderMap::new();
        h.insert(
            "authorization",
            HeaderValue::from_static("Basic Zm9vOmJhcg=="),
        );
        assert!(extract_bearer_token(&h).is_err());
    }

    #[test]
    fn constant_time_eq_known_vectors() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abx"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
    }
}
