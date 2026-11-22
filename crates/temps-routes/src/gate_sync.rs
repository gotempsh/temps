// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Node-facing half of the Temps gate (ADR 065 D8).
//!
//! - `POST /internal/nodes/{id}/gate/redeem`: an agent exchanges a sign-in
//!   code for a gate token. Only for an address in that agent's own route
//!   snapshot, so a code seen in a log or a `Referer` is useless without the
//!   state cookie *and* the token of a node that serves the address.
//! - `POST /internal/nodes/{id}/gate/logout`: an agent revokes the session
//!   behind a token a visitor logged out with.
//! - `POST /internal/nodes/{id}/gate/access-keys`: an agent met a Cloudflare
//!   Access JWT signed with a key it does not have (ADR 065 D9), and asks
//!   for the team's keys again. Rate-limited per team by
//!   [`AccessKeyService`](crate::access_keys::AccessKeyService).

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use temps_core::gate::{GateAccessKeysRequest, GateLogoutRequest, GateRedeemRequest};
use temps_entities::nodes;
use tracing::{error, warn};

use crate::gate::GateServiceError;
use crate::route_sync::{authenticate_node, request_declares_gate, RouteSyncAppState};

fn refusal(error: GateServiceError) -> (StatusCode, String) {
    let status =
        StatusCode::from_u16(error.http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    if status.is_server_error() {
        error!(%error, "gate request from a node failed");
    }
    (status, error.to_string())
}

/// Whether `node` is given the signed-in address `address_id` in its
/// snapshot: the request declares the gate (as a snapshot request must to
/// receive one), the node takes public traffic (directly or through a
/// connector), and the address is one of the worker routes.
async fn node_serves_address(
    state: &RouteSyncAppState,
    headers: &HeaderMap,
    node: &nodes::Model,
    address_id: &str,
) -> Result<bool, (StatusCode, String)> {
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
    let internal = |operation: &str, error: sea_orm::DbErr| {
        error!(node_id = node.id, %error, "{operation} failed");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to {operation} for node {}: {error}", node.id),
        )
    };
    if !request_declares_gate(headers) {
        return Ok(false);
    }
    let runs_connector = temps_entities::ingress_connector_nodes::Entity::find()
        .filter(temps_entities::ingress_connector_nodes::Column::NodeId.eq(node.id))
        .one(state.db.as_ref())
        .await
        .map_err(|e| internal("check ingress connector assignment", e))?
        .is_some();
    if !node.public_ingress_enabled && !runs_connector {
        return Ok(false);
    }
    let routes = crate::route_sync::worker_public_routes(
        state.db.as_ref(),
        &state.peer_table,
        !state.request_policy_gate.supports_worker_ingress(),
        crate::route_sync::request_declares(headers, temps_core::worker_shield::SHIELD_CAPABILITY),
    )
    .await
    .map_err(|(operation, source)| internal(operation, source))?;
    Ok(routes.routes.iter().any(|route| {
        route
            .gate
            .as_ref()
            .is_some_and(|gate| gate.address_id == address_id)
    }))
}

pub async fn post_gate_redeem(
    State(state): State<Arc<RouteSyncAppState>>,
    headers: HeaderMap,
    Path(node_id): Path<i32>,
    Json(request): Json<GateRedeemRequest>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let node = authenticate_node(&state.db, &headers, node_id).await?;
    if !node_serves_address(&state, &headers, &node, &request.address_id).await? {
        warn!(
            node_id,
            address_id = %request.address_id,
            "node tried to redeem a sign-in code for an address it does not serve"
        );
        return Err((
            StatusCode::FORBIDDEN,
            format!(
                "Node {node_id} does not serve address {}, so it cannot redeem its sign-in codes",
                request.address_id
            ),
        ));
    }
    let redeemed = state.gate.redeem(&request).await.map_err(refusal)?;
    Ok(Json(redeemed))
}

pub async fn post_gate_logout(
    State(state): State<Arc<RouteSyncAppState>>,
    headers: HeaderMap,
    Path(node_id): Path<i32>,
    Json(request): Json<GateLogoutRequest>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let node = authenticate_node(&state.db, &headers, node_id).await?;
    if !node_serves_address(&state, &headers, &node, &request.address_id).await? {
        return Err((
            StatusCode::FORBIDDEN,
            format!(
                "Node {node_id} does not serve address {}, so it cannot sign anyone out of it",
                request.address_id
            ),
        ));
    }
    state.gate.logout(&request).await.map_err(refusal)?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn post_gate_access_keys(
    State(state): State<Arc<RouteSyncAppState>>,
    headers: HeaderMap,
    Path(node_id): Path<i32>,
    Json(request): Json<GateAccessKeysRequest>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let node = authenticate_node(&state.db, &headers, node_id).await?;
    if !node_serves_address(&state, &headers, &node, &request.address_id).await? {
        return Err((
            StatusCode::FORBIDDEN,
            format!(
                "Node {node_id} does not serve address {}, so it cannot ask for its Access keys",
                request.address_id
            ),
        ));
    }
    let provider_id = state
        .access_keys
        .provider_of_address(&request.address_id)
        .await
        .map_err(|error| {
            error!(node_id, address_id = %request.address_id, %error, "could not find the Access token of an address");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(
                    "Failed to find the Cloudflare Access token of address {}: {error}",
                    request.address_id
                ),
            )
        })?
        .ok_or_else(|| {
            (
                StatusCode::CONFLICT,
                format!(
                    "Address {} is not protected by Cloudflare Access",
                    request.address_id
                ),
            )
        })?;
    match state.access_keys.refresh(provider_id, false).await {
        Ok(_) => Ok(StatusCode::NO_CONTENT),
        Err(error) => {
            warn!(node_id, provider_id, %error, "could not fetch Cloudflare Access keys an agent asked for");
            Err((StatusCode::BAD_GATEWAY, error.to_string()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refusals_map_to_client_and_server_errors() {
        let address_id = "custom:1".to_string();
        assert_eq!(
            refusal(GateServiceError::InvalidCode {
                address_id: address_id.clone()
            })
            .0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            refusal(GateServiceError::NotGated {
                address_id: address_id.clone(),
                hostname: "a.example.test".into()
            })
            .0,
            StatusCode::CONFLICT
        );
        assert_eq!(
            refusal(GateServiceError::Key {
                operation: "unsealed",
                reason: "x".into()
            })
            .0,
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }
}
