// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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
//! and never has to reconcile partial state. The generation is claimed
//! from the durable `route_generation` row by this process alone (see
//! `RouteGenerationRole`), so it continues across CP restarts; a `since`
//! that differs from the current generation (e.g. after a database
//! restore) returns at once and the agent re-fetches.
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
use thiserror::Error;
use tracing::{error, warn};

use crate::route_table::{BackendType, CachedPeerTable, RouteInfo};

/// Hold the long-poll request open this long before returning the
/// current snapshot when no generation bump arrives. Slightly less
/// than typical idle-timeout boundaries on intermediate proxies.
const LONG_POLL_TIMEOUT: Duration = Duration::from_secs(25);

pub struct RouteSyncAppState {
    pub db: Arc<DatabaseConnection>,
    pub peer_table: Arc<CachedPeerTable>,
    pub encryption_service: Arc<temps_core::EncryptionService>,
    pub request_policy_gate: Arc<temps_core::RequestPolicyGateSlot>,
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
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RouteBackendDto {
    pub address: String,
    pub container_id: Option<String>,
    pub container_name: Option<String>,
}

/// One internal-zone route. Workers index by `host` (lower-cased) and
/// pick from `backends`. `deployment_id` is sent back through the
/// proxy chain as a header for log correlation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
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
    pub public_ingress: PublicIngressSnapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PublicIngressSnapshot {
    pub enabled: bool,
    pub routes: Vec<RouteEntryDto>,
    pub unsupported_route_count: usize,
    pub unsupported_reasons: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub certificates: Option<PublicIngressCertificates>,
    /// Where the agent binds its public listeners (ADR 058 D7). Absent when
    /// the node takes no direct traffic (off, or a connector only); agents
    /// that predate it keep using `--public-ingress-address`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listen: Option<PublicListenDto>,
    /// Addresses only this worker's private listener serves (ADR 064 D5),
    /// each with the range a visitor must come from. Never in `routes`, so
    /// direct listeners and tunnels never answer them; agents that predate
    /// private mode ignore the field and serve none of them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub private_routes: Vec<PrivateRouteEntryDto>,
}

/// A private address on a worker's private listener (ADR 064 D5).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PrivateRouteEntryDto {
    #[serde(flatten)]
    pub route: RouteEntryDto,
    /// What the worker's inside check is told to check, instead of a host.
    pub address_id: String,
    /// The network a visitor must come from, by TCP peer address.
    pub range: String,
    /// The listener address DNS points at: what the inside check compares
    /// the hostname's answer with and dials.
    pub listener: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PublicListenDto {
    /// Local address to bind; `None` means the default-route interface.
    pub address: Option<String>,
    pub http_port: u16,
    pub https_port: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublicIngressCertificates {
    pub ephemeral_public_key: String,
    pub bundles: Vec<PublicIngressCertBundle>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublicIngressCertBundle {
    pub domain: String,
    pub ciphertext: String,
    pub nonce: String,
    pub fingerprint: String,
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

#[derive(Debug, Deserialize)]
pub struct AcmeChallengeQuery {
    pub host: String,
    pub token: String,
}

#[derive(Debug, Serialize)]
pub struct AcmeChallengeResponse {
    pub key_authorization: String,
}

/// Projects and environments that store a rate-limit policy. Only the
/// control-plane proxy enforces rate limits (#1288), so any such policy keeps
/// public ingress on the control plane. Counts stored policies regardless of
/// the security switch: over-counting only forgoes direct worker ingress,
/// while under-counting would serve a limited project unlimited.
async fn count_project_rate_limit_policies(db: &DatabaseConnection) -> Result<i64, sea_orm::DbErr> {
    use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
    let row = db
        .query_one(Statement::from_string(
            DatabaseBackend::Postgres,
            "SELECT \
               (SELECT COUNT(*) FROM projects \
                 WHERE deleted_at IS NULL \
                   AND jsonb_typeof(deployment_config -> 'security' -> 'rateLimiting') = 'object') \
             + (SELECT COUNT(*) FROM environments \
                 WHERE deleted_at IS NULL \
                   AND jsonb_typeof(deployment_config -> 'security' -> 'rateLimiting') = 'object') \
             AS policies",
        ))
        .await?;
    match row {
        Some(row) => row.try_get::<i64>("", "policies"),
        None => Ok(0),
    }
}

#[derive(Debug, Error)]
enum SnapshotBuildError {
    #[error("Failed to {operation} for node {node_id}: {source}")]
    Database {
        node_id: i32,
        operation: &'static str,
        source: sea_orm::DbErr,
    },
    #[error("Node {node_id} has an invalid public-ingress encryption key: {reason}")]
    InvalidEncryptionKey { node_id: i32, reason: String },
    #[error("Failed to decrypt certificate for domain '{domain}' for node {node_id}: {reason}")]
    CertificateDecrypt {
        node_id: i32,
        domain: String,
        reason: String,
    },
    #[error("Failed to encrypt certificate for domain '{domain}' for node {node_id}: {reason}")]
    CertificateEncrypt {
        node_id: i32,
        domain: String,
        reason: String,
    },
}

#[derive(Debug, Error)]
enum AcmeChallengeError {
    #[error("Public ingress is disabled for node {node_id}")]
    Disabled { node_id: i32 },
    #[error("Invalid ACME challenge lookup for node {node_id}: {reason}")]
    Invalid { node_id: i32, reason: &'static str },
    #[error("Failed to query ACME challenge for node {node_id}, host '{host}': {source}")]
    Database {
        node_id: i32,
        host: String,
        source: sea_orm::DbErr,
    },
    #[error("ACME challenge was not found for node {node_id}, host '{host}'")]
    NotFound { node_id: i32, host: String },
}

fn acme_error_response(error: AcmeChallengeError) -> (StatusCode, String) {
    let status = match error {
        AcmeChallengeError::Disabled { .. } => StatusCode::FORBIDDEN,
        AcmeChallengeError::Invalid { .. } => StatusCode::BAD_REQUEST,
        AcmeChallengeError::NotFound { .. } => StatusCode::NOT_FOUND,
        AcmeChallengeError::Database { .. } => StatusCode::INTERNAL_SERVER_ERROR,
    };
    error!(%error, "ACME challenge lookup failed");
    (
        status,
        status
            .canonical_reason()
            .unwrap_or("Request failed")
            .to_string(),
    )
}

fn snapshot_error_response(error: SnapshotBuildError) -> (StatusCode, String) {
    error!(%error, "failed to build worker route snapshot");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "Failed to build worker route snapshot".to_string(),
    )
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
    let node = authenticate_node(&app_state.db, &headers, node_id).await?;

    // Arm the notifier BEFORE checking the generation. `Notify::notified()`
    // captures the current notify epoch at construction time, so a reload
    // that races between our check and the wait is still observed instead
    // of being silently missed — the same lost-wakeup hazard
    // `CachedPeerTable::wait_until_loaded` already avoids the same way.
    // Constructing the `Notified` future only *after* the fast-path check
    // (the previous shape of this function) leaves a window where a
    // `load_routes()` reload that lands in that window is invisible to
    // this request: `notify_waiters()` only wakes waiters already armed
    // when it's called, so we'd then wait out the full
    // `LONG_POLL_TIMEOUT` (25s) instead of returning almost immediately.
    // `mark_deployment_complete`'s worker-apply gate only allows 10s, so
    // any occurrence of this race causes a spurious deployment revert even
    // though the worker is perfectly healthy.
    let notifier = app_state.peer_table.generation_notifier();
    let armed = notifier.notified();
    tokio::pin!(armed);

    // Fast path: generation already moved past `since` — return now. A
    // `since` ahead of this server (its generation was reset, e.g. a restore)
    // also returns now: waiting for the counter to exceed it would leave the
    // agent on stale routes, unwoken by every reload, for the full poll.
    let mut current = app_state.peer_table.current_generation();
    if current != q.since {
        return Ok(Json(
            build_snapshot(&app_state, &node, current)
                .await
                .map_err(snapshot_error_response)?,
        ));
    }

    // Slow path: park on the notifier until either a reload happens
    // or the long-poll deadline fires. We don't block forever — many
    // intermediate proxies (and the agent's HTTP client) drop idle
    // connections after ~30 s, so we return a same-generation snapshot
    // before that point and let the agent reconnect cleanly.
    let _ = tokio::time::timeout(LONG_POLL_TIMEOUT, async {
        // First wait uses the future armed above (before the fast-path
        // check) so it can't miss a reload that raced the check.
        armed.await;
        // Loop guards against spurious wakeups: keep waiting until the
        // generation actually moves. Subsequent iterations arm fresh —
        // by this point we're already inside the wait, so a race here
        // only costs another loop iteration, bounded by the outer timeout.
        loop {
            if app_state.peer_table.current_generation() != q.since {
                break;
            }
            let notified = notifier.notified();
            tokio::pin!(notified);
            notified.await;
        }
    })
    .await;
    current = app_state.peer_table.current_generation();
    // Reload the node: the wake-up is usually a change to it (public
    // ingress turned on or off, a new ingress key), and the row read before
    // the wait would answer it with the old state for a whole poll. This
    // also drops a token revoked while the request was parked.
    let node = authenticate_node(&app_state.db, &headers, node_id).await?;
    Ok(Json(
        build_snapshot(&app_state, &node, current)
            .await
            .map_err(snapshot_error_response)?,
    ))
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
    //
    // A generation this server has not issued (an agent still on a snapshot
    // from before a reset) proves nothing about the current routes; storing
    // it would satisfy the completion gate for routes the node never
    // received. Its next poll returns at once with the current snapshot.
    if body.applied_generation > server_generation {
        warn!(
            node_id,
            applied_generation = body.applied_generation,
            server_generation,
            "ignoring route ACK for a generation this server has not issued"
        );
        return Ok(Json(RouteAckResponse {
            node_id,
            applied_generation: body.applied_generation,
            server_generation,
        }));
    }
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

pub async fn get_acme_challenge(
    State(app_state): State<Arc<RouteSyncAppState>>,
    headers: HeaderMap,
    Path(node_id): Path<i32>,
    Query(query): Query<AcmeChallengeQuery>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let node = authenticate_node(&app_state.db, &headers, node_id).await?;
    let key_authorization =
        lookup_acme_challenge(app_state.db.as_ref(), &node, &query.host, &query.token)
            .await
            .map_err(acme_error_response)?;
    Ok(Json(AcmeChallengeResponse { key_authorization }))
}

async fn lookup_acme_challenge(
    db: &DatabaseConnection,
    node: &nodes::Model,
    requested_host: &str,
    token: &str,
) -> Result<String, AcmeChallengeError> {
    use sea_orm::{ColumnTrait, QueryFilter};
    use temps_entities::domains;
    if !node.public_ingress_enabled {
        return Err(AcmeChallengeError::Disabled { node_id: node.id });
    }
    let host = requested_host.trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty() || token.is_empty() || token.len() > 512 {
        return Err(AcmeChallengeError::Invalid {
            node_id: node.id,
            reason: "host and token must be non-empty and token must not exceed 512 bytes",
        });
    }
    domains::Entity::find()
        .filter(domains::Column::Domain.eq(host.clone()))
        .filter(domains::Column::HttpChallengeToken.eq(token))
        .one(db)
        .await
        .map_err(|source| AcmeChallengeError::Database {
            node_id: node.id,
            host: host.clone(),
            source,
        })?
        .and_then(|domain| domain.http_challenge_key_authorization)
        .ok_or(AcmeChallengeError::NotFound {
            node_id: node.id,
            host,
        })
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
        .route(
            "/internal/nodes/{node_id}/acme-challenge",
            get(get_acme_challenge),
        )
        .route(
            "/internal/nodes/{node_id}/public-ingress/key",
            post(crate::public_ingress_sync::post_ingress_key),
        )
        .route(
            "/internal/nodes/{node_id}/public-ingress/report",
            post(crate::public_ingress_sync::post_listener_report),
        )
}

// ---------------------------------------------------------------------------
// Snapshot builder
// ---------------------------------------------------------------------------

/// The public routes workers serve themselves, and why the others stay on
/// the control plane. The route snapshot and the ingress connectors (ADR 058)
/// share it, so a tunnel never publishes a hostname its workers would not
/// serve.
#[derive(Debug, Clone, Default)]
pub struct WorkerPublicRoutes {
    pub routes: Vec<RouteEntryDto>,
    /// Routes offered to workers that a policy or a control-plane backend
    /// keeps on the control plane.
    pub unsupported_route_count: usize,
    pub unsupported_reasons: Vec<String>,
    /// Cluster-wide policies that keep every route on the control plane;
    /// empty when only some routes are held back.
    pub blocking_policies: Vec<String>,
}

/// What keeps routes off workers: cluster-wide policies only the
/// control-plane proxy enforces, and the addresses of registered workers a
/// worker-served backend must be on.
pub struct WorkerRoutePolicy {
    pub blocking_policies: Vec<String>,
    pub registered_worker_addresses: std::collections::HashSet<std::net::IpAddr>,
}

pub async fn worker_route_policy(
    db: &DatabaseConnection,
    custom_request_policy: bool,
) -> Result<WorkerRoutePolicy, (&'static str, sea_orm::DbErr)> {
    use sea_orm::{ColumnTrait, PaginatorTrait, QueryFilter};
    use temps_entities::{ip_access_control, project_ip_access_rules, settings};
    let app_settings = settings::Entity::find()
        .one(db)
        .await
        .map_err(|source| ("load public-ingress request policy", source))?
        .map(|row| temps_core::AppSettings::from_json(row.data))
        .unwrap_or_default();
    let ip_policy_count = ip_access_control::Entity::find()
        .count(db)
        .await
        .map_err(|source| ("inspect public-ingress IP policy", source))?;
    // Per-project / per-environment allow and deny lists are enforced by
    // the control-plane proxy only; a worker ingress node has no copy of
    // them, so serving those hosts there would bypass the list.
    let project_ip_rule_count = project_ip_access_rules::Entity::find()
        .filter(project_ip_access_rules::Column::Mode.ne("disabled"))
        .count(db)
        .await
        .map_err(|source| ("inspect public-ingress project IP access rules", source))?;
    let project_rate_limit_count = count_project_rate_limit_policies(db)
        .await
        .map_err(|source| ("inspect project rate-limit policies", source))?;
    let registered_worker_addresses: std::collections::HashSet<std::net::IpAddr> =
        nodes::Entity::find()
            .filter(nodes::Column::Role.eq("worker"))
            .filter(nodes::Column::Status.eq("active"))
            .all(db)
            .await
            .map_err(|source| ("load registered worker backend addresses", source))?
            .into_iter()
            .filter_map(|worker| worker.data_address().parse().ok())
            .collect();
    let mut blocking_policies = Vec::new();
    if app_settings.rate_limiting.enabled {
        blocking_policies.push("global rate limiting is enabled".to_string());
    }
    if app_settings.security_headers.enabled {
        blocking_policies.push("global security headers are enabled".to_string());
    }
    if project_rate_limit_count > 0 {
        blocking_policies.push("project rate limits are configured".to_string());
    }
    if ip_policy_count > 0 {
        blocking_policies.push("IP access-control rules are configured".to_string());
    }
    if project_ip_rule_count > 0 {
        blocking_policies.push("project IP access rules are configured".to_string());
    }
    if custom_request_policy {
        blocking_policies
            .push("a custom request-policy provider requires control-plane ingress".to_string());
    }
    Ok(WorkerRoutePolicy {
        blocking_policies,
        registered_worker_addresses,
    })
}

pub async fn worker_public_routes(
    db: &DatabaseConnection,
    peer_table: &CachedPeerTable,
    custom_request_policy: bool,
) -> Result<WorkerPublicRoutes, (&'static str, sea_orm::DbErr)> {
    let WorkerRoutePolicy {
        blocking_policies,
        registered_worker_addresses,
    } = worker_route_policy(db, custom_request_policy).await?;
    let public_route_count = peer_table.worker_public_route_count();
    let raw_public_routes = if blocking_policies.is_empty() {
        peer_table.snapshot_worker_public_routes()
    } else {
        Vec::new()
    };
    let routes: Vec<RouteEntryDto> = raw_public_routes
        .into_iter()
        .filter_map(|(host, route)| entry_from_route(host, &route))
        .filter(|route| {
            route.backends.iter().all(|backend| {
                public_backend_is_registered_worker(&backend.address, &registered_worker_addresses)
            })
        })
        .collect();
    let mut unsupported_reasons = blocking_policies.clone();
    if public_route_count > routes.len() && unsupported_reasons.is_empty() {
        unsupported_reasons.push("some routes require redirects, static-file serving, wake-up, attack mode, project security policy, project-group path routing, or a remotely reachable backend".to_string());
    }
    Ok(WorkerPublicRoutes {
        unsupported_route_count: public_route_count.saturating_sub(routes.len()),
        routes,
        unsupported_reasons,
        blocking_policies,
    })
}

/// The listener a worker's private ingress is bound on (ADR 064 D5), as it
/// reported: `None` unless it reports `private` mode and is bound on a
/// private address. The control plane never routes a private address to a
/// listener on a public one, whatever the worker says.
pub fn private_listener_of(
    row: Option<&temps_entities::node_public_ingress::Model>,
) -> Option<std::net::IpAddr> {
    let row = row?;
    if row.mode.as_deref() != Some(temps_core::worker_ingress::IngressMode::Private.as_str()) {
        return None;
    }
    let https = row.listening.as_ref()?.get("https")?.as_str()?;
    let ip = temps_core::worker_ingress::parse_reported_ip(https)?;
    temps_core::worker_ingress::check_private_listen(ip).ok()?;
    Some(ip)
}

/// The private addresses `node_id` serves (ADR 064 D5), and why any it was
/// given are held back.
pub fn worker_private_routes(
    peer_table: &CachedPeerTable,
    policy: &WorkerRoutePolicy,
    node_id: i32,
) -> (Vec<PrivateRouteEntryDto>, Vec<String>) {
    let mut routes = Vec::new();
    let mut held_back = Vec::new();
    for (host, private, servable) in peer_table.snapshot_worker_private_routes(node_id) {
        let why = if let Some(policy) = policy.blocking_policies.first() {
            Some(format!("{policy}, which only the control plane enforces"))
        } else if !servable {
            Some("it needs a redirect, static files, wake-up, attack mode, security settings or path routing, which only the control plane serves".to_string())
        } else {
            None
        };
        let range = private
            .address
            .range
            .clone()
            // A range that does not parse, or reaches past one private
            // block, admits nobody rather than everybody.
            .filter(|range| temps_core::ways_in::cidr_is_private(range) == Some(true));
        let listener = private.address.listener.clone();
        let entry = entry_from_route(host.clone(), &private.route).filter(|entry| {
            entry.backends.iter().all(|backend| {
                public_backend_is_registered_worker(
                    &backend.address,
                    &policy.registered_worker_addresses,
                )
            })
        });
        match (why, range, listener, entry) {
            (Some(why), ..) => held_back.push(format!("{host}: {why}")),
            (None, None, ..) => held_back.push(format!(
                "{host}: it has no private network range; make it reachable again"
            )),
            (None, _, None, _) => held_back.push(format!("{host}: it has no listener address")),
            (None, _, _, None) => held_back.push(format!(
                "{host}: its backends are not on a worker this one can reach"
            )),
            (None, Some(range), Some(listener), Some(route)) => routes.push(PrivateRouteEntryDto {
                route,
                address_id: private.address.address_id.clone(),
                range,
                listener,
            }),
        }
    }
    routes.sort_by(|a, b| a.route.host.cmp(&b.route.host));
    (routes, held_back)
}

async fn build_snapshot(
    state: &RouteSyncAppState,
    node: &nodes::Model,
    generation: u64,
) -> Result<RouteSnapshot, SnapshotBuildError> {
    let raw = state.peer_table.snapshot_internal_routes();
    let mut routes = Vec::with_capacity(raw.len());
    for (host, info) in raw {
        if let Some(entry) = entry_from_route(host, &info) {
            routes.push(entry);
        }
    }
    // A node running an ingress connector (ADR 058) serves public routes on
    // its loopback tunnel origin even without a public listener.
    let runs_connector = {
        use sea_orm::{ColumnTrait, QueryFilter};
        temps_entities::ingress_connector_nodes::Entity::find()
            .filter(temps_entities::ingress_connector_nodes::Column::NodeId.eq(node.id))
            .one(state.db.as_ref())
            .await
            .map_err(|source| SnapshotBuildError::Database {
                node_id: node.id,
                operation: "check ingress connector assignment",
                source,
            })?
            .is_some()
    };
    // A worker in private mode (ADR 064 D5) gets the private addresses
    // recorded for it, and only while it is bound on a private address.
    let private_listener = {
        let row = temps_entities::node_public_ingress::Entity::find_by_id(node.id)
            .one(state.db.as_ref())
            .await
            .map_err(|source| SnapshotBuildError::Database {
                node_id: node.id,
                operation: "load private ingress state",
                source,
            })?;
        private_listener_of(row.as_ref())
    };
    let (private_routes, private_held_back) = if private_listener.is_some() {
        let policy = worker_route_policy(
            state.db.as_ref(),
            !state.request_policy_gate.supports_worker_ingress(),
        )
        .await
        .map_err(|(operation, source)| SnapshotBuildError::Database {
            node_id: node.id,
            operation,
            source,
        })?;
        worker_private_routes(&state.peer_table, &policy, node.id)
    } else {
        (Vec::new(), Vec::new())
    };
    let public_ingress =
        if !private_routes.is_empty() && !node.public_ingress_enabled && !runs_connector {
            let hosts: std::collections::HashSet<String> = private_routes
                .iter()
                .map(|route| route.route.host.trim_end_matches('.').to_ascii_lowercase())
                .collect();
            PublicIngressSnapshot {
                enabled: true,
                routes: Vec::new(),
                unsupported_route_count: private_held_back.len(),
                unsupported_reasons: private_held_back,
                certificates: encrypt_public_certificates(state, node, &hosts).await?,
                listen: None,
                private_routes,
            }
        } else if node.public_ingress_enabled || runs_connector {
            let WorkerPublicRoutes {
                routes: public_routes,
                unsupported_route_count,
                unsupported_reasons,
                ..
            } = worker_public_routes(
                state.db.as_ref(),
                &state.peer_table,
                !state.request_policy_gate.supports_worker_ingress(),
            )
            .await
            .map_err(|(operation, source)| SnapshotBuildError::Database {
                node_id: node.id,
                operation,
                source,
            })?;
            // A node gets keys only for what it terminates itself: its public
            // routes when it listens publicly, and its private routes. A
            // connector-only node terminates none of the public ones.
            let allowed_certificate_hosts: std::collections::HashSet<String> = public_routes
                .iter()
                .filter(|_| node.public_ingress_enabled)
                .map(|route| route.host.trim_end_matches('.').to_ascii_lowercase())
                .chain(
                    private_routes
                        .iter()
                        .map(|route| route.route.host.trim_end_matches('.').to_ascii_lowercase()),
                )
                .collect();
            let mut unsupported_reasons = unsupported_reasons;
            unsupported_reasons.extend(private_held_back.iter().cloned());
            PublicIngressSnapshot {
                enabled: true,
                routes: public_routes,
                unsupported_route_count,
                unsupported_reasons,
                // Only a node that terminates TLS itself gets private keys. A
                // connector serves plain HTTP on loopback and the provider holds
                // the certificate, so running one is no reason to ship keys.
                certificates: if node.public_ingress_enabled || !private_routes.is_empty() {
                    encrypt_public_certificates(state, node, &allowed_certificate_hosts).await?
                } else {
                    None
                },
                listen: if node.public_ingress_enabled {
                    Some(public_listen(state, node).await?)
                } else {
                    None
                },
                private_routes,
            }
        } else {
            PublicIngressSnapshot::default()
        };
    Ok(RouteSnapshot {
        generation,
        routes,
        public_ingress,
    })
}

async fn public_listen(
    state: &RouteSyncAppState,
    node: &nodes::Model,
) -> Result<PublicListenDto, SnapshotBuildError> {
    let row = temps_entities::node_public_ingress::Entity::find_by_id(node.id)
        .one(state.db.as_ref())
        .await
        .map_err(|source| SnapshotBuildError::Database {
            node_id: node.id,
            operation: "load public ingress listen settings",
            source,
        })?;
    let port = |value: i32, default: u16| {
        u16::try_from(value)
            .ok()
            .filter(|p| *p != 0)
            .unwrap_or(default)
    };
    Ok(match row {
        Some(row) => PublicListenDto {
            address: row.listen_address,
            http_port: port(row.http_port, 80),
            https_port: port(row.https_port, 443),
        },
        None => PublicListenDto {
            address: None,
            http_port: 80,
            https_port: 443,
        },
    })
}

fn public_backend_is_registered_worker(
    address: &str,
    registered_worker_addresses: &std::collections::HashSet<std::net::IpAddr>,
) -> bool {
    address
        .parse::<std::net::SocketAddr>()
        .is_ok_and(|address| {
            !address.ip().is_loopback()
                && !address.ip().is_unspecified()
                && registered_worker_addresses.contains(&address.ip())
        })
}

async fn encrypt_public_certificates(
    state: &RouteSyncAppState,
    node: &nodes::Model,
    allowed: &std::collections::HashSet<String>,
) -> Result<Option<PublicIngressCertificates>, SnapshotBuildError> {
    use sea_orm::{ColumnTrait, QueryFilter};
    use temps_entities::domains;

    let Some(public_key) = node.edge_public_key.as_deref() else {
        return Ok(None);
    };
    let session = temps_core::ecies::EncryptionSession::new(public_key).map_err(|error| {
        warn!(node_id = node.id, %error, "invalid worker ingress public key");
        SnapshotBuildError::InvalidEncryptionKey {
            node_id: node.id,
            reason: error.to_string(),
        }
    })?;
    let domain_rows = domains::Entity::find()
        .filter(domains::Column::Status.is_in(domains::CERT_SERVING_STATUSES))
        .all(state.db.as_ref())
        .await
        .map_err(|source| SnapshotBuildError::Database {
            node_id: node.id,
            operation: "load public-ingress certificates",
            source,
        })?;
    let mut bundles = Vec::new();
    for domain in domain_rows {
        let certificate_domain = domain.domain.trim_end_matches('.').to_ascii_lowercase();
        // A wildcard private key authorizes every one-label sibling, including
        // protected or unsupported routes that are deliberately absent from
        // this worker's snapshot. Export only exact leaf certificates so a
        // compromised worker cannot impersonate another tenant under the
        // same managed suffix.
        let allowed_certificate =
            !certificate_domain.starts_with("*.") && allowed.contains(&certificate_domain);
        if !allowed_certificate {
            continue;
        }
        let (Some(certificate), Some(encrypted_key)) = (domain.certificate, domain.private_key)
        else {
            continue;
        };
        if !certificate_has_single_exact_dns_identity(&certificate, &certificate_domain) {
            warn!(
                node_id = node.id,
                domain = %certificate_domain,
                "refusing to export certificate with wildcard, additional, or non-DNS identities"
            );
            continue;
        }
        let key = state
            .encryption_service
            .decrypt_string(&encrypted_key)
            .map_err(|error| SnapshotBuildError::CertificateDecrypt {
                node_id: node.id,
                domain: domain.domain.clone(),
                reason: error.to_string(),
            })?;
        let encrypted = session
            .encrypt(format!("{certificate}\n{key}").as_bytes())
            .map_err(|error| SnapshotBuildError::CertificateEncrypt {
                node_id: node.id,
                domain: domain.domain.clone(),
                reason: error.to_string(),
            })?;
        bundles.push(PublicIngressCertBundle {
            domain: domain.domain,
            ciphertext: encrypted.ciphertext,
            nonce: encrypted.nonce,
            fingerprint: temps_core::ecies::cert_fingerprint(&certificate),
        });
    }
    if bundles.is_empty() {
        Ok(None)
    } else {
        Ok(Some(PublicIngressCertificates {
            ephemeral_public_key: session.ephemeral_public_key().to_string(),
            bundles,
        }))
    }
}

fn certificate_has_single_exact_dns_identity(certificate_pem: &str, expected: &str) -> bool {
    use x509_parser::extensions::GeneralName;
    let mut reader = std::io::BufReader::new(certificate_pem.as_bytes());
    let Some(Ok(certificate)) = rustls_pemfile::certs(&mut reader).next() else {
        return false;
    };
    let Ok((_, parsed)) = x509_parser::parse_x509_certificate(certificate.as_ref()) else {
        return false;
    };
    let Ok(Some(san)) = parsed.subject_alternative_name() else {
        return false;
    };
    san.value.general_names.len() == 1
        && san.value.general_names.iter().all(|name| match name {
            GeneralName::DNSName(name) => {
                !name.starts_with("*.") && name.eq_ignore_ascii_case(expected)
            }
            _ => false,
        })
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

pub async fn authenticate_node(
    db: &DatabaseConnection,
    headers: &HeaderMap,
    node_id: i32,
) -> Result<nodes::Model, (StatusCode, String)> {
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
    if node.status != "active" {
        return Err((
            StatusCode::UNAUTHORIZED,
            format!("Node {} is not active", node_id),
        ));
    }
    Ok(node)
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
    hex::encode(hasher.finalize())
}

/// Constant-time equality for the SHA-256 token hash check.
///
/// Delegates to `subtle::ConstantTimeEq`, which is the same primitive used
/// by Stripe webhook signature verification in `temps-revenue`. The hand-
/// rolled XOR loop this replaced returned early on length mismatch — for
/// fixed-length hashes (always 64 hex chars in our case) the practical
/// risk was minimal, but the new impl removes the timing channel entirely
/// and is also auditor-friendly (no DIY crypto).
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    use subtle::ConstantTimeEq;
    a.ct_eq(b).into()
}

#[cfg(test)]
pub(crate) mod tests {
    use std::collections::HashSet;
    use std::sync::atomic::AtomicUsize;

    use sea_orm::{ActiveModelTrait, DatabaseBackend, DbErr, MockDatabase};
    use temps_database::test_utils::TestDatabase;
    use temps_entities::domains;

    use super::*;
    use crate::route_table::BackendEntry;
    use crate::test_utils::TestDBMockOperations;

    pub(crate) fn test_node(public_ingress_enabled: bool) -> nodes::Model {
        let now = chrono::Utc::now();
        nodes::Model {
            id: 7,
            name: "worker-test".to_string(),
            token_hash: "test-token-hash".to_string(),
            token_encrypted: None,
            address: "https://192.0.2.7:3100".to_string(),
            private_address: "192.0.2.7".to_string(),
            public_endpoint: None,
            wg_public_key: None,
            role: "worker".to_string(),
            status: "active".to_string(),
            labels: serde_json::json!({}),
            capacity: serde_json::json!({}),
            last_heartbeat: Some(now),
            edge_public_key: None,
            compute_cidr: None,
            architecture: None,
            underlay_address: None,
            mesh_wg_public_key: None,
            mesh_wg_endpoint: None,
            mesh_wg_address: None,
            dns_resolver_running: None,
            dns_resolver_tasks_alive: None,
            dns_resolver_last_sync_at: None,
            dns_resolver_consecutive_failures: 0,
            dns_resolver_last_error: None,
            dns_resolver_record_count: None,
            build_limits: None,
            failover_at: None,
            public_ingress_enabled,
            public_ingress_running: None,
            public_ingress_last_error: None,
            public_ingress_certificate_count: None,
            public_ingress_route_count: None,
            public_ingress_unsupported_route_count: None,
            public_ingress_unsupported_reasons: serde_json::json!([]),
            occupied_ranges: None,
            overlay_status: None,
            overlay_last_error: None,
            overlay_ready_cidr: None,
            overlay_remediation: None,
            overlay_network_name: None,
            created_at: now,
            updated_at: now,
        }
    }

    struct RestrictiveRequestPolicy;

    impl temps_core::RequestPolicyGate for RestrictiveRequestPolicy {
        fn evaluate(
            &self,
            _context: &temps_core::RequestPolicyContext<'_>,
        ) -> temps_core::RequestPolicyDecision {
            temps_core::RequestPolicyDecision::Deny {
                reason: "test policy",
                rule_id: None,
                revision: None,
            }
        }
    }

    #[tokio::test]
    async fn worker_snapshot_exports_public_route_only_for_ready_open_request_policy(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let test_database = match TestDatabase::with_migrations().await {
            Ok(database) => database,
            Err(error)
                if std::env::var_os("TEMPS_TEST_DATABASE_URL").is_none()
                    && temps_database::test_utils::is_container_runtime_unavailable(
                        &error.to_string(),
                    ) =>
            {
                eprintln!("skipping worker snapshot test: test database unavailable: {error}");
                return Ok(());
            }
            Err(error) => return Err(error.into()),
        };
        let test_db = TestDBMockOperations::new(test_database.db.clone()).await?;
        let (project, environment, deployment) = test_db
            .create_test_project_with_domain("worker-public.example.test")
            .await?;
        let project_id = project.id;
        let node = test_node(true);
        nodes::ActiveModel::from(node.clone())
            .insert(test_db.db.as_ref())
            .await?;

        let peer_table = Arc::new(CachedPeerTable::new(test_db.db.clone()));
        peer_table.insert_route_for_test(
            "worker-public.example.test",
            RouteInfo {
                backend: BackendType::Upstream {
                    backends: vec![BackendEntry {
                        address: "192.0.2.7:32000".to_string(),
                        container_id: Some("worker-container".to_string()),
                        container_name: Some("worker-app".to_string()),
                    }],
                    round_robin_counter: Arc::new(AtomicUsize::new(0)),
                },
                redirect_to: None,
                status_code: None,
                project: Some(Arc::new(project)),
                environment: Some(Arc::new(environment)),
                deployment: Some(Arc::new(deployment)),
                cert_eligible: true,
            },
        );
        let encryption_service = Arc::new(temps_core::EncryptionService::new(&"11".repeat(32))?);

        let open_gate = Arc::new(temps_core::RequestPolicyGateSlot::new_default());
        assert!(open_gate.set(Arc::new(temps_core::OpenRequestPolicyGate)));
        let open_snapshot = build_snapshot(
            &RouteSyncAppState {
                db: test_db.db.clone(),
                peer_table: Arc::clone(&peer_table),
                encryption_service: Arc::clone(&encryption_service),
                request_policy_gate: open_gate,
            },
            &node,
            1,
        )
        .await?;
        assert_eq!(open_snapshot.public_ingress.routes.len(), 1);
        assert_eq!(
            open_snapshot.public_ingress.routes[0].host,
            "worker-public.example.test"
        );
        assert_eq!(
            open_snapshot.public_ingress.routes[0].backends[0].address,
            "192.0.2.7:32000"
        );
        assert!(open_snapshot.public_ingress.unsupported_reasons.is_empty());

        for request_policy_gate in [
            Arc::new(temps_core::RequestPolicyGateSlot::new_default()),
            {
                let slot = Arc::new(temps_core::RequestPolicyGateSlot::new_default());
                assert!(slot.set(Arc::new(RestrictiveRequestPolicy)));
                slot
            },
        ] {
            let snapshot = build_snapshot(
                &RouteSyncAppState {
                    db: test_db.db.clone(),
                    peer_table: Arc::clone(&peer_table),
                    encryption_service: Arc::clone(&encryption_service),
                    request_policy_gate,
                },
                &node,
                1,
            )
            .await?;
            assert!(snapshot.public_ingress.routes.is_empty());
            assert_eq!(snapshot.public_ingress.unsupported_route_count, 1);
            assert!(snapshot
                .public_ingress
                .unsupported_reasons
                .iter()
                .any(|reason| {
                    reason == "a custom request-policy provider requires control-plane ingress"
                }));
        }

        // A project-level rate limit is only enforced by the control-plane
        // proxy (#1288), so it must also keep public ingress there.
        assert_eq!(
            count_project_rate_limit_policies(test_db.db.as_ref()).await?,
            0
        );
        {
            use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
            test_db
                .db
                .execute(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    "UPDATE projects SET deployment_config = jsonb_set(\
                       COALESCE(deployment_config, '{}'::jsonb), '{security}', \
                       '{\"enabled\": true, \"rateLimiting\": {\"maxRequestsPerMinute\": 2}}'::jsonb) \
                     WHERE id = $1",
                    [project_id.into()],
                ))
                .await?;
        }
        assert_eq!(
            count_project_rate_limit_policies(test_db.db.as_ref()).await?,
            1
        );
        let open_gate = Arc::new(temps_core::RequestPolicyGateSlot::new_default());
        assert!(open_gate.set(Arc::new(temps_core::OpenRequestPolicyGate)));
        let limited_snapshot = build_snapshot(
            &RouteSyncAppState {
                db: test_db.db.clone(),
                peer_table: Arc::clone(&peer_table),
                encryption_service: Arc::clone(&encryption_service),
                request_policy_gate: open_gate,
            },
            &node,
            1,
        )
        .await?;
        assert!(limited_snapshot.public_ingress.routes.is_empty());
        assert!(limited_snapshot
            .public_ingress
            .unsupported_reasons
            .iter()
            .any(|reason| reason == "project rate limits are configured"));

        // Per-project IP allow/deny lists likewise stay on the control plane,
        // but a `disabled` row must not.
        {
            use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
            test_db
                .db
                .execute(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    "UPDATE projects SET deployment_config = '{}'::jsonb WHERE id = $1",
                    [project_id.into()],
                ))
                .await?;
            test_db
                .db
                .execute(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    "INSERT INTO project_ip_access_rules (project_id, mode, cidrs) \
                     VALUES ($1, 'disabled', '[]'::jsonb)",
                    [project_id.into()],
                ))
                .await?;
        }
        let build_with_open_gate = || async {
            let open_gate = Arc::new(temps_core::RequestPolicyGateSlot::new_default());
            assert!(open_gate.set(Arc::new(temps_core::OpenRequestPolicyGate)));
            build_snapshot(
                &RouteSyncAppState {
                    db: test_db.db.clone(),
                    peer_table: Arc::clone(&peer_table),
                    encryption_service: Arc::clone(&encryption_service),
                    request_policy_gate: open_gate,
                },
                &node,
                1,
            )
            .await
        };
        let disabled_snapshot = build_with_open_gate().await?;
        assert_eq!(disabled_snapshot.public_ingress.routes.len(), 1);
        {
            use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
            test_db
                .db
                .execute(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    "UPDATE project_ip_access_rules SET mode = 'allow', \
                     cidrs = '[{\"cidr\": \"100.64.0.0/10\"}]'::jsonb WHERE project_id = $1",
                    [project_id.into()],
                ))
                .await?;
        }
        let guarded_snapshot = build_with_open_gate().await?;
        assert!(guarded_snapshot.public_ingress.routes.is_empty());
        assert!(guarded_snapshot
            .public_ingress
            .unsupported_reasons
            .iter()
            .any(|reason| reason == "project IP access rules are configured"));

        test_db.cleanup().await?;
        Ok(())
    }

    fn challenge_domain(
        host: &str,
        token: &str,
        key_authorization: Option<&str>,
    ) -> domains::Model {
        let now = chrono::Utc::now();
        domains::Model {
            id: 11,
            domain: host.to_string(),
            certificate: None,
            private_key: None,
            expiration_time: None,
            last_renewed: None,
            status: "challenge_requested".to_string(),
            dns_challenge_token: None,
            dns_challenge_value: None,
            http_challenge_token: Some(token.to_string()),
            http_challenge_key_authorization: key_authorization.map(str::to_string),
            last_error: None,
            last_error_type: None,
            is_wildcard: false,
            verification_method: "http-01".to_string(),
            on_demand_backoff_until: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn certificate_with_sans(sans: &[&str]) -> String {
        let ca = temps_core::node_pki::generate_cluster_ca().expect("generate test CA");
        let csr = temps_core::node_pki::generate_node_keypair_csr(
            "certificate-scope-test",
            &["ignored.example.test".to_string()],
        )
        .expect("generate test certificate request");
        temps_core::node_pki::sign_node_csr(
            &ca.cert_pem,
            &ca.key_pem,
            &csr.csr_pem,
            &sans.iter().map(|san| san.to_string()).collect::<Vec<_>>(),
        )
        .expect("sign test certificate")
        .cert_pem
    }

    #[tokio::test]
    async fn test_lookup_acme_challenge_enabled_known_host_and_token_returns_key_authorization() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![challenge_domain(
                "pending.example.test",
                "challenge-token",
                Some("challenge-token.thumbprint"),
            )]])
            .into_connection();

        let result = lookup_acme_challenge(
            &db,
            &test_node(true),
            "PENDING.EXAMPLE.TEST.",
            "challenge-token",
        )
        .await;

        assert_eq!(result.unwrap(), "challenge-token.thumbprint");
    }

    #[tokio::test]
    async fn test_lookup_acme_challenge_disabled_returns_disabled_without_querying_database() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();

        let result = lookup_acme_challenge(
            &db,
            &test_node(false),
            "pending.example.test",
            "challenge-token",
        )
        .await;

        assert!(matches!(
            result.unwrap_err(),
            AcmeChallengeError::Disabled { node_id: 7 }
        ));
    }

    #[tokio::test]
    async fn test_lookup_acme_challenge_unknown_host_or_token_returns_not_found() {
        for (host, token) in [
            ("unknown.example.test", "challenge-token"),
            ("pending.example.test", "unknown-token"),
        ] {
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([Vec::<domains::Model>::new()])
                .into_connection();

            let result = lookup_acme_challenge(&db, &test_node(true), host, token).await;

            assert!(matches!(
                result.unwrap_err(),
                AcmeChallengeError::NotFound { node_id: 7, host: error_host }
                    if error_host == host
            ));
        }
    }

    #[tokio::test]
    async fn test_lookup_acme_challenge_database_failure_returns_contextual_database_error() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_errors([DbErr::Custom("database unavailable".to_string())])
            .into_connection();

        let result = lookup_acme_challenge(
            &db,
            &test_node(true),
            "pending.example.test",
            "challenge-token",
        )
        .await;

        assert!(matches!(
            result.unwrap_err(),
            AcmeChallengeError::Database { node_id: 7, host, source }
                if host == "pending.example.test"
                    && source.to_string().contains("database unavailable")
        ));
    }

    #[test]
    fn test_public_backend_is_registered_worker_accepts_only_registered_routable_addresses() {
        let registered = HashSet::from([
            "192.0.2.10".parse().unwrap(),
            "2001:db8::10".parse().unwrap(),
        ]);

        assert!(public_backend_is_registered_worker(
            "192.0.2.10:32000",
            &registered
        ));
        assert!(public_backend_is_registered_worker(
            "[2001:db8::10]:32000",
            &registered
        ));
    }

    #[test]
    fn test_public_backend_is_registered_worker_rejects_untrusted_addresses() {
        let registered = HashSet::from([
            "127.0.0.1".parse().unwrap(),
            "0.0.0.0".parse().unwrap(),
            "192.0.2.10".parse().unwrap(),
        ]);

        for address in [
            "127.0.0.1:32000",
            "0.0.0.0:32000",
            "192.0.2.11:32000",
            "not-a-socket-address",
        ] {
            assert!(
                !public_backend_is_registered_worker(address, &registered),
                "untrusted backend {address} was accepted"
            );
        }
    }

    #[test]
    fn test_certificate_has_single_exact_dns_identity_accepts_exact_leaf() {
        let certificate = certificate_with_sans(&["app.example.test"]);

        assert!(certificate_has_single_exact_dns_identity(
            &certificate,
            "app.example.test"
        ));
    }

    #[test]
    fn test_certificate_has_single_exact_dns_identity_rejects_additional_sibling() {
        let certificate = certificate_with_sans(&["app.example.test", "protected.example.test"]);

        assert!(!certificate_has_single_exact_dns_identity(
            &certificate,
            "app.example.test"
        ));
    }

    #[test]
    fn test_certificate_has_single_exact_dns_identity_rejects_wildcard() {
        let certificate = certificate_with_sans(&["*.example.test"]);

        assert!(!certificate_has_single_exact_dns_identity(
            &certificate,
            "app.example.test"
        ));
    }
}
