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
    extract::{Path, State},
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
use tracing::{error, warn};
use utoipa::ToSchema;

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

/// Response body for `GET /internal/nodes/{node_id}/network/peers`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PeerListResponse {
    /// Caller's own allocation, or `null` if multi-host networking has
    /// not been enabled for this node yet.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alloc: Option<AllocEntry>,
    /// All other nodes with a `compute_cidr` set, excluding the caller.
    pub peers: Vec<PeerEntry>,
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
    let token = extract_bearer_token(&headers)?;
    let node = app_state
        .node_service
        .get_by_id(node_id)
        .await
        .map_err(Problem::from)?;
    let token_hash = sha256_hash(&token);
    if !constant_time_eq(node.token_hash.as_bytes(), token_hash.as_bytes()) {
        warn!(node_id, "Invalid network/peers token");
        return Err(problemdetails::new(StatusCode::UNAUTHORIZED)
            .with_title("Invalid Token")
            .with_detail(format!("Invalid authentication token for node {}", node_id)));
    }

    // ----- 2. Self-alloc + peers -----
    let allocator = PostgresAllocator::new(app_state.db.clone());
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

    Ok(Json(PeerListResponse { alloc, peers }))
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
            alloc: None,
            peers: vec![],
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(!json.contains("alloc"), "alloc should be omitted: {}", json);
        assert!(json.contains("\"peers\":[]"));
    }

    #[test]
    fn response_includes_alloc_when_present() {
        let resp = PeerListResponse {
            alloc: Some(AllocEntry {
                node_id: "abc".into(),
                compute_cidr: "172.20.1.0/24".into(),
                bridge_address: "172.20.1.1".into(),
                underlay_address: "10.0.0.1".into(),
            }),
            peers: vec![],
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"alloc\""));
        assert!(json.contains("172.20.1.0/24"));
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
}
