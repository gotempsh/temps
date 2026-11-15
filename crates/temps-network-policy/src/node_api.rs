// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! `POST /internal/nodes/{node_id}/network-policy`: a worker agent's
//! enforcer report in, its mode and, outside `legacy`, its policy snapshot
//! out.
//!
//! Mounted next to route sync, outside the plugin router: it is
//! authenticated by the node token, not a user session. The agent calls it
//! on every enforcer pass, in `legacy` too. It says which datapath it
//! enforces with, which is what lets an operator move the node out of
//! `legacy` (see [`NetworkPolicyService::supports_enforcement`]), and which
//! workloads it runs, which is how the other nodes learn their addresses
//! (see [`NetworkPolicyService::record_observed`]).

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use axum::{Json, Router};
use sea_orm::DatabaseConnection;
use temps_workload_netpol::{NodePolicy, NodePolicyReport};
use tracing::error;

use crate::service::NetworkPolicyService;

pub struct NetworkPolicyNodeState {
    pub db: Arc<DatabaseConnection>,
    pub service: Arc<NetworkPolicyService>,
}

pub fn node_routes(state: Arc<NetworkPolicyNodeState>) -> Router {
    Router::new()
        .route(
            "/internal/nodes/{node_id}/network-policy",
            post(node_policy),
        )
        .with_state(state)
}

async fn node_policy(
    State(state): State<Arc<NetworkPolicyNodeState>>,
    Path(node_id): Path<i32>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<NodePolicy>, (StatusCode, String)> {
    // Authenticated before the body is parsed: an unauthenticated caller
    // never gets the server to decode anything.
    temps_routes::route_sync::authenticate_node(&state.db, &headers, node_id).await?;
    let report: NodePolicyReport = serde_json::from_slice(&body).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("network policy report from node {node_id} is not valid JSON: {e}"),
        )
    })?;
    state
        .service
        .node_policy(node_id, report)
        .await
        .map(Json)
        .map_err(|e| {
            error!(node_id, error = %e, "could not build the node's network policy");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("network policy for node {node_id}: {e}"),
            )
        })
}
