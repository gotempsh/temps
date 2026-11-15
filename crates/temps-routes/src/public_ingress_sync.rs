// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Node-facing half of direct worker ingress (ADR 058 D7).
//!
//! - `POST /internal/nodes/{id}/public-ingress/key`: a worker enrolled before
//!   worker ingress registers the public half of the key certificate bundles
//!   are encrypted to, instead of being rejoined.
//! - `POST /internal/nodes/{id}/public-ingress/report`: the host addresses the
//!   agent can bind, and what it bound, for the console.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;
use temps_core::worker_ingress::ListenerReport;
use tracing::{error, info};

use crate::public_ingress_store::{KeyRegistration, PublicIngressStore};
use crate::route_sync::{authenticate_node, RouteSyncAppState};

#[derive(Debug, Deserialize)]
pub struct RegisterIngressKeyRequest {
    /// Base64 X25519 public key, as `temps join` sends at enrollment.
    pub public_key: String,
}

fn internal(node_id: i32, what: &str, error: impl std::fmt::Display) -> (StatusCode, String) {
    error!(node_id, %error, "{what} failed");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        format!("Failed to {what} for node {node_id}: {error}"),
    )
}

pub async fn post_ingress_key(
    State(state): State<Arc<RouteSyncAppState>>,
    headers: HeaderMap,
    Path(node_id): Path<i32>,
    Json(body): Json<RegisterIngressKeyRequest>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let node = authenticate_node(&state.db, &headers, node_id).await?;
    let public_key = body.public_key.trim().to_string();
    if let Err(error) = temps_core::ecies::EncryptionSession::new(&public_key) {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("Ingress key for node {node_id} is not a valid X25519 public key: {error}"),
        ));
    }
    let store = PublicIngressStore::new(state.db.as_ref());
    match store
        .register_edge_key(&node, &public_key)
        .await
        .map_err(|e| internal(node_id, "store the ingress key", e))?
    {
        KeyRegistration::AlreadyRegistered => return Ok(StatusCode::NO_CONTENT),
        KeyRegistration::Refused => return Err((StatusCode::CONFLICT, replace_refused(node_id))),
        KeyRegistration::Stored => {}
    }
    info!(node_id, "worker registered its ingress key");
    // Certificate bundles are encrypted to this key: send a fresh snapshot.
    store
        .announce_change()
        .await
        .map_err(|e| internal(node_id, "announce the ingress key", e))?;
    Ok(StatusCode::NO_CONTENT)
}

fn replace_refused(node_id: i32) -> String {
    format!(
        "Node {node_id} already has an ingress key, and a node cannot replace it on its own. \
         If this worker lost its key (its data directory was wiped), re-enroll it: create a join \
         command in Settings → Worker Nodes and run `temps join` on the worker, which registers \
         a new key."
    )
}

pub async fn post_listener_report(
    State(state): State<Arc<RouteSyncAppState>>,
    headers: HeaderMap,
    Path(node_id): Path<i32>,
    Json(report): Json<ListenerReport>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    authenticate_node(&state.db, &headers, node_id).await?;
    PublicIngressStore::new(state.db.as_ref())
        .record_listener_report(node_id, report)
        .await
        .map_err(|e| internal(node_id, "store the ingress listener report", e))?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refused_key_says_how_to_recover() {
        assert!(replace_refused(7).contains("temps join"));
    }
}
