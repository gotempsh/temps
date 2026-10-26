// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Shield on workers, control-plane side (ADR 066, D10, I3, I5 and I8).
//!
//! - Each worker route carries its environment's Shield settings
//!   ([`route_policy`]): the effective mode, the exempt paths and the hashes
//!   of the bypass tokens valid for it. The route snapshot names the current
//!   Shield snapshot revision; it does not carry the body, which would ride
//!   along every 25 s poll.
//! - `GET /internal/nodes/{id}/shield/snapshot?revision=N` returns the
//!   newest stored body when it differs from `N` (else `304`), with the
//!   challenge keys sealed to the node, so a worker issues and verifies the
//!   same tokens and clearances as the control plane.
//! - `POST /internal/nodes/{id}/shield/events` takes a worker's events, gzip
//!   JSON, and hands them to the control plane's event sink.
//!
//! Both endpoints take the node's bearer token, like every other internal
//! node endpoint, and answer only a node that takes public traffic.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder, QuerySelect};
use temps_core::worker_shield::{
    SealedShieldKeys, ShieldEventsAccepted, ShieldKeyEntry, ShieldKeyMaterial, ShieldRoutePolicy,
    ShieldSnapshotQuery, ShieldSnapshotResponse, MAX_EVENTS_DECOMPRESSED_BYTES,
    MAX_EVENTS_PER_UPLOAD, SHIELD_KEYS_INFO,
};
use temps_entities::{nodes, shield_bypass_tokens, shield_snapshots};
use temps_shield::{events, NoEvents, ShieldEvent, ShieldKeys};

/// The sink type the proxy registers in the plugin service registry, for
/// callers that build [`ShieldEventSinkSlot`] without depending on Shield.
pub use temps_shield::EventSink;
use thiserror::Error;
use tracing::{debug, error, warn};

use crate::route_sync::{authenticate_node, RouteSyncAppState};
use crate::route_table::{route_shield_mode, RouteInfo, ShieldPinning};

/// Most reasons one event keeps; a verdict rarely has more than a few.
const MAX_EVENT_REASONS: usize = 32;

/// The hashes of the bypass tokens that are not revoked, by project.
#[derive(Debug, Default)]
pub struct BypassTokens {
    /// `(environment, hash)`; a `None` environment is valid for the whole
    /// project.
    by_project: HashMap<i32, Vec<(Option<i32>, String)>>,
}

impl BypassTokens {
    /// One query, for every project: the snapshot needs them for every
    /// route it sends.
    pub async fn load(db: &DatabaseConnection) -> Result<Self, sea_orm::DbErr> {
        let rows: Vec<(i32, Option<i32>, String)> = shield_bypass_tokens::Entity::find()
            .select_only()
            .column(shield_bypass_tokens::Column::ProjectId)
            .column(shield_bypass_tokens::Column::EnvironmentId)
            .column(shield_bypass_tokens::Column::TokenHash)
            .filter(shield_bypass_tokens::Column::RevokedAt.is_null())
            .into_tuple()
            .all(db)
            .await?;
        Ok(Self::from_rows(rows))
    }

    pub fn from_rows(rows: impl IntoIterator<Item = (i32, Option<i32>, String)>) -> Self {
        let mut by_project: HashMap<i32, Vec<(Option<i32>, String)>> = HashMap::new();
        for (project_id, environment_id, hash) in rows {
            by_project
                .entry(project_id)
                .or_default()
                .push((environment_id, hash));
        }
        Self { by_project }
    }

    /// The hashes valid for `environment_id`: its own and its project's.
    pub fn for_environment(&self, project_id: i32, environment_id: i32) -> Vec<String> {
        let mut hashes: Vec<String> = self
            .by_project
            .get(&project_id)
            .into_iter()
            .flatten()
            .filter(|(environment, _)| environment.is_none_or(|id| id == environment_id))
            .map(|(_, hash)| hash.clone())
            .collect();
        hashes.sort();
        hashes.dedup();
        hashes
    }
}

/// What a worker applies to `route` (ADR 066, D10): `None` for a route
/// without a project and an environment, which has no Shield settings.
pub fn route_policy(route: &RouteInfo, tokens: &BypassTokens) -> Option<ShieldRoutePolicy> {
    let project = route.project.as_ref()?;
    let environment = route.environment.as_ref()?;
    Some(ShieldRoutePolicy {
        project_id: project.id,
        environment_id: environment.id,
        mode: route_shield_mode(route).as_str().to_string(),
        exempt_paths: environment.shield_exempt_paths.0.clone(),
        bypass_token_hashes: tokens.for_environment(project.id, environment.id),
    })
}

/// The newest stored Shield snapshot revision; 0 when none is stored.
pub async fn newest_revision(db: &DatabaseConnection) -> Result<u64, sea_orm::DbErr> {
    let revision: Option<i64> = shield_snapshots::Entity::find()
        .select_only()
        .column(shield_snapshots::Column::Revision)
        .order_by_desc(shield_snapshots::Column::Revision)
        .into_tuple()
        .one(db)
        .await?;
    Ok(revision.map_or(0, |revision| u64::try_from(revision).unwrap_or(0)))
}

/// The challenge keys, sealed to the node's ingress key under
/// [`SHIELD_KEYS_INFO`]: each key the gate derives from the master key,
/// under its label, so the worker's gate signs what the control plane's
/// verifies, and the reverse.
pub fn seal_keys(
    encryption: &temps_core::EncryptionService,
    node_public_key_b64: &str,
) -> Result<SealedShieldKeys, temps_core::ecies::EciesError> {
    let entries = RefCell::new(Vec::new());
    ShieldKeys::derive(|label| {
        let key = encryption.derive_subkey(label);
        entries.borrow_mut().push(ShieldKeyEntry {
            label: label.to_string(),
            key: BASE64.encode(key),
        });
        key
    });
    let material = ShieldKeyMaterial {
        keys: entries.into_inner(),
    };
    let plaintext = serde_json::to_vec(&material).map_err(|error| {
        temps_core::ecies::EciesError::EncryptionFailed(format!(
            "serializing the Shield keys: {error}"
        ))
    })?;
    let session =
        temps_core::ecies::EncryptionSession::with_info(node_public_key_b64, SHIELD_KEYS_INFO)?;
    let bundle = session.encrypt(&plaintext)?;
    Ok(SealedShieldKeys {
        ephemeral_public_key: session.ephemeral_public_key().to_string(),
        ciphertext: bundle.ciphertext,
        nonce: bundle.nonce,
    })
}

/// Where workers' events go: the control plane's [`EventSink`], registered
/// in the plugin service registry by the proxy's event store. Resolved on
/// first use, since this state is built before every plugin is; until one
/// is registered events are dropped, with one warning.
pub struct ShieldEventSinkSlot {
    resolve: Box<dyn Fn() -> Option<Arc<dyn EventSink>> + Send + Sync>,
    resolved: OnceLock<Arc<dyn EventSink>>,
    warned: AtomicBool,
}

impl ShieldEventSinkSlot {
    pub fn new(resolve: impl Fn() -> Option<Arc<dyn EventSink>> + Send + Sync + 'static) -> Self {
        Self {
            resolve: Box::new(resolve),
            resolved: OnceLock::new(),
            warned: AtomicBool::new(false),
        }
    }

    /// Always `sink`.
    pub fn fixed(sink: Arc<dyn EventSink>) -> Self {
        Self::new(move || Some(Arc::clone(&sink)))
    }

    fn sink(&self) -> Arc<dyn EventSink> {
        if let Some(sink) = self.resolved.get() {
            return Arc::clone(sink);
        }
        match (self.resolve)() {
            Some(sink) => Arc::clone(self.resolved.get_or_init(|| sink)),
            None => {
                if !self.warned.swap(true, Ordering::Relaxed) {
                    warn!(
                        "no Shield event sink is registered; events workers upload are dropped \
                         until one is"
                    );
                }
                Arc::new(NoEvents)
            }
        }
    }
}

#[derive(Debug, Error)]
enum ShieldSyncError {
    #[error("Failed to {operation} for node {node_id}: {source}")]
    Database {
        node_id: i32,
        operation: &'static str,
        source: sea_orm::DbErr,
    },
    #[error("Node {node_id} takes no public traffic, so it runs no Shield")]
    NoPublicTraffic { node_id: i32 },
    #[error("Shield events from node {node_id} are not gzip: {reason}")]
    NotGzip { node_id: i32, reason: String },
    #[error("Shield events from node {node_id} decompress past {limit} bytes")]
    TooLarge { node_id: i32, limit: usize },
    #[error("Node {node_id} sent {count} Shield events in one upload; at most {limit} are taken")]
    TooMany {
        node_id: i32,
        count: usize,
        limit: usize,
    },
    #[error("Shield events from node {node_id} are not a JSON array of events: {reason}")]
    Malformed { node_id: i32, reason: String },
}

fn error_response(error: ShieldSyncError) -> (StatusCode, String) {
    let status = match &error {
        ShieldSyncError::Database { .. } => StatusCode::INTERNAL_SERVER_ERROR,
        ShieldSyncError::NoPublicTraffic { .. } => StatusCode::FORBIDDEN,
        ShieldSyncError::NotGzip { .. } | ShieldSyncError::Malformed { .. } => {
            StatusCode::BAD_REQUEST
        }
        ShieldSyncError::TooLarge { .. } | ShieldSyncError::TooMany { .. } => {
            StatusCode::PAYLOAD_TOO_LARGE
        }
    };
    if status.is_server_error() {
        error!(%error, "Shield sync request failed");
    } else {
        warn!(%error, "Shield sync request refused");
    }
    (status, error.to_string())
}

/// Shield runs on a worker's public listeners and tunnel origin; a node
/// with neither has no use for the keys, and is not given them.
async fn require_public_traffic(
    db: &DatabaseConnection,
    node: &nodes::Model,
) -> Result<(), ShieldSyncError> {
    if node.public_ingress_enabled {
        return Ok(());
    }
    let runs_connector = temps_entities::ingress_connector_nodes::Entity::find()
        .filter(temps_entities::ingress_connector_nodes::Column::NodeId.eq(node.id))
        .one(db)
        .await
        .map_err(|source| ShieldSyncError::Database {
            node_id: node.id,
            operation: "check ingress connector assignment",
            source,
        })?
        .is_some();
    if runs_connector {
        Ok(())
    } else {
        Err(ShieldSyncError::NoPublicTraffic { node_id: node.id })
    }
}

pub async fn get_shield_snapshot(
    State(state): State<Arc<RouteSyncAppState>>,
    headers: HeaderMap,
    Path(node_id): Path<i32>,
    Query(query): Query<ShieldSnapshotQuery>,
) -> Result<Response, (StatusCode, String)> {
    let node = authenticate_node(&state.db, &headers, node_id).await?;
    require_public_traffic(state.db.as_ref(), &node)
        .await
        .map_err(error_response)?;
    let database = |operation| {
        move |source| {
            error_response(ShieldSyncError::Database {
                node_id,
                operation,
                source,
            })
        }
    };
    let newest = newest_revision(state.db.as_ref())
        .await
        .map_err(database("load the newest Shield snapshot revision"))?;
    // A worker on the newest revision has it, and its keys (it fetched them
    // with it, or asks with revision 0 when it has none in memory).
    if query.revision != 0 && query.revision == newest {
        return Ok(StatusCode::NOT_MODIFIED.into_response());
    }
    let row = if newest == 0 {
        None
    } else {
        shield_snapshots::Entity::find_by_id(newest as i64)
            .one(state.db.as_ref())
            .await
            .map_err(database("load the newest Shield snapshot"))?
    };
    let keys = match node.edge_public_key.as_deref() {
        Some(public_key) => match seal_keys(&state.encryption_service, public_key) {
            Ok(keys) => Some(keys),
            Err(error) => {
                warn!(node_id, %error, "could not seal the Shield keys to the node's ingress key");
                None
            }
        },
        None => {
            debug!(
                node_id,
                "node has no ingress key yet; sending the Shield snapshot without keys"
            );
            None
        }
    };
    let body = match row {
        Some(row) => ShieldSnapshotResponse {
            revision: u64::try_from(row.revision).unwrap_or(0),
            sha256: Some(row.sha256),
            body: Some(BASE64.encode(&row.body)),
            keys,
        },
        // Pruned between the two reads, or none stored: the built-in lists.
        None => ShieldSnapshotResponse {
            revision: 0,
            sha256: None,
            body: None,
            keys,
        },
    };
    let mut response = Json(body).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

/// `body` gunzipped, refusing anything that inflates past `limit` bytes.
fn gunzip_bounded(node_id: i32, body: &[u8], limit: usize) -> Result<Vec<u8>, ShieldSyncError> {
    let mut decoded = Vec::new();
    flate2::read::GzDecoder::new(body)
        .take(limit as u64 + 1)
        .read_to_end(&mut decoded)
        .map_err(|error| ShieldSyncError::NotGzip {
            node_id,
            reason: error.to_string(),
        })?;
    if decoded.len() > limit {
        return Err(ShieldSyncError::TooLarge { node_id, limit });
    }
    Ok(decoded)
}

/// What a worker reported, made safe to store: the node is the one that
/// authenticated, whatever the event says, and every string is bounded
/// again, since the worker's bound is not this side's guarantee.
fn sanitize(mut event: ShieldEvent, node_id: i32) -> ShieldEvent {
    event.node_id = Some(node_id);
    event.request_id = events::bounded(&event.request_id);
    event.ip = events::bounded(&event.ip);
    event.ja4 = event.ja4.as_deref().map(events::bounded);
    event.verdict = events::bounded(&event.verdict);
    event.mode = events::bounded(&event.mode);
    event.reasons.truncate(MAX_EVENT_REASONS);
    for reason in &mut event.reasons {
        *reason = events::bounded(reason);
    }
    event.signature = event.signature.as_deref().map(events::bounded);
    event.host = events::bounded(&event.host);
    event.path = events::bounded(&event.path);
    event.user_agent = events::bounded(&event.user_agent);
    event
}

/// The `(project, environment)` pairs a worker running Shield may serve:
/// an event for any other is not one this node saw on its own routes.
fn servable_environments(state: &RouteSyncAppState) -> HashSet<(i32, i32)> {
    state
        .peer_table
        .snapshot_worker_public_routes(ShieldPinning::None)
        .into_iter()
        .filter_map(|(_, route)| Some((route.project.as_ref()?.id, route.environment.as_ref()?.id)))
        .collect()
}

pub async fn post_shield_events(
    State(state): State<Arc<RouteSyncAppState>>,
    headers: HeaderMap,
    Path(node_id): Path<i32>,
    body: Bytes,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let node = authenticate_node(&state.db, &headers, node_id).await?;
    require_public_traffic(state.db.as_ref(), &node)
        .await
        .map_err(error_response)?;
    let decoded =
        gunzip_bounded(node_id, &body, MAX_EVENTS_DECOMPRESSED_BYTES).map_err(error_response)?;
    let events: Vec<ShieldEvent> = serde_json::from_slice(&decoded).map_err(|error| {
        error_response(ShieldSyncError::Malformed {
            node_id,
            reason: error.to_string(),
        })
    })?;
    if events.len() > MAX_EVENTS_PER_UPLOAD {
        return Err(error_response(ShieldSyncError::TooMany {
            node_id,
            count: events.len(),
            limit: MAX_EVENTS_PER_UPLOAD,
        }));
    }
    let received = events.len();
    let servable = servable_environments(&state);
    let events: Vec<ShieldEvent> = events
        .into_iter()
        .filter(|event| servable.contains(&(event.project_id, event.environment_id)))
        .map(|event| sanitize(event, node_id))
        .collect();
    if events.len() < received {
        debug!(
            node_id,
            dropped = received - events.len(),
            "dropped Shield events for environments workers do not serve"
        );
    }
    let accepted = events.len();
    if accepted > 0 {
        state.shield_events.sink().submit(events);
    }
    Ok((
        StatusCode::ACCEPTED,
        Json(ShieldEventsAccepted { accepted }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Mutex;

    #[test]
    fn tokens_for_an_environment_are_its_own_and_its_projects() {
        let tokens = BypassTokens::from_rows([
            (1, None, "aa".to_string()),
            (1, Some(10), "bb".to_string()),
            (1, Some(11), "cc".to_string()),
            (2, None, "dd".to_string()),
        ]);
        assert_eq!(tokens.for_environment(1, 10), ["aa", "bb"]);
        assert_eq!(tokens.for_environment(1, 11), ["aa", "cc"]);
        assert_eq!(tokens.for_environment(2, 20), ["dd"]);
        assert!(tokens.for_environment(3, 30).is_empty());
    }

    #[test]
    fn sealed_keys_open_only_with_the_nodes_key_and_info() {
        let encryption = temps_core::EncryptionService::new(&"11".repeat(32)).unwrap();
        let (private, public) = temps_core::worker_ingress::generate_ingress_key().unwrap();
        let sealed = seal_keys(&encryption, &public).unwrap();
        let bundle = temps_core::ecies::EncryptedBundle {
            ciphertext: sealed.ciphertext.clone(),
            nonce: sealed.nonce.clone(),
        };
        let plain = temps_core::ecies::decrypt_bundle_with_info(
            &private,
            &sealed.ephemeral_public_key,
            &bundle,
            SHIELD_KEYS_INFO,
        )
        .unwrap();
        let material: ShieldKeyMaterial = serde_json::from_slice(&plain).unwrap();
        let challenge = material
            .keys
            .iter()
            .find(|entry| entry.label == "temps-shield/challenge/k1")
            .expect("the current challenge key");
        assert_eq!(
            BASE64.decode(&challenge.key).unwrap(),
            encryption.derive_subkey("temps-shield/challenge/k1")
        );
        // Never openable as a certificate bundle.
        assert!(
            temps_core::ecies::decrypt_bundle(&private, &sealed.ephemeral_public_key, &bundle)
                .is_err()
        );
    }

    #[test]
    fn uploads_are_bounded_when_inflated() {
        use flate2::write::GzEncoder;
        use std::io::Write;
        let gzip = |data: &[u8]| {
            let mut encoder = GzEncoder::new(Vec::new(), flate2::Compression::fast());
            encoder.write_all(data).unwrap();
            encoder.finish().unwrap()
        };
        assert_eq!(gunzip_bounded(7, &gzip(b"[]"), 16).unwrap(), b"[]");
        // A small body that inflates past the limit.
        let bomb = gzip(&vec![b' '; 1024 * 1024]);
        assert!(bomb.len() < 8 * 1024);
        assert!(matches!(
            gunzip_bounded(7, &bomb, 1024),
            Err(ShieldSyncError::TooLarge { node_id: 7, .. })
        ));
        assert!(matches!(
            gunzip_bounded(7, b"[]", 1024),
            Err(ShieldSyncError::NotGzip { .. })
        ));
    }

    fn event(node_id: Option<i32>) -> ShieldEvent {
        ShieldEvent {
            timestamp_ms: 1,
            project_id: 1,
            environment_id: 10,
            node_id,
            request_id: "r".repeat(2000),
            kind: temps_shield::EventKind::Verdict,
            ip: "203.0.113.9".into(),
            ja4: None,
            verdict: "block".into(),
            mode: "selective".into(),
            reasons: vec!["x".repeat(2000); 100],
            signature: None,
            host: "app.example.test".into(),
            path: "/".repeat(2000),
            user_agent: "ua".into(),
            difficulty: None,
            solve_ms: None,
        }
    }

    #[test]
    fn events_name_the_node_that_sent_them_and_stay_bounded() {
        let clean = sanitize(event(Some(99)), 7);
        assert_eq!(clean.node_id, Some(7));
        assert_eq!(clean.request_id.chars().count(), events::MAX_FIELD_CHARS);
        assert_eq!(clean.path.chars().count(), events::MAX_FIELD_CHARS);
        assert_eq!(clean.reasons.len(), MAX_EVENT_REASONS);
        assert!(clean
            .reasons
            .iter()
            .all(|reason| reason.chars().count() == events::MAX_FIELD_CHARS));
    }

    #[derive(Default)]
    struct Collect {
        events: Mutex<Vec<ShieldEvent>>,
    }

    impl EventSink for Collect {
        fn submit(&self, events: Vec<ShieldEvent>) {
            self.events.lock().unwrap().extend(events);
        }
    }

    #[test]
    fn the_sink_is_resolved_once_registered_and_absent_drops() {
        let calls = Arc::new(AtomicUsize::new(0));
        let registered: Arc<Mutex<Option<Arc<dyn EventSink>>>> = Arc::new(Mutex::new(None));
        let slot = {
            let calls = Arc::clone(&calls);
            let registered = Arc::clone(&registered);
            ShieldEventSinkSlot::new(move || {
                calls.fetch_add(1, Ordering::Relaxed);
                registered.lock().unwrap().clone()
            })
        };
        slot.sink().submit(vec![event(None)]);
        let collect = Arc::new(Collect::default());
        *registered.lock().unwrap() = Some(collect.clone());
        slot.sink().submit(vec![event(None)]);
        slot.sink().submit(vec![event(None)]);
        assert_eq!(collect.events.lock().unwrap().len(), 2);
        assert_eq!(calls.load(Ordering::Relaxed), 2, "resolved, then kept");
    }

    fn gzip_json<T: serde::Serialize>(value: &T) -> Bytes {
        use flate2::write::GzEncoder;
        use std::io::Write;
        let mut encoder = GzEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder
            .write_all(&serde_json::to_vec(value).unwrap())
            .unwrap();
        Bytes::from(encoder.finish().unwrap())
    }

    fn bearer(token: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
        );
        headers
    }

    /// The route data a `shield-v1` agent gets (policy, Shield routes, the
    /// revision) and an older one does not, and both endpoints: their
    /// authentication, `304`, the size caps, and events reaching the sink
    /// for the node that sent them only.
    #[tokio::test]
    async fn workers_get_shield_routes_snapshots_and_send_events(
    ) -> Result<(), Box<dyn std::error::Error>> {
        use crate::route_sync::tests::test_node;
        use crate::route_sync::{build_snapshot, worker_public_routes, AgentCapabilities};
        use crate::route_table::{BackendEntry, BackendType, CachedPeerTable};
        use sea_orm::{ActiveModelTrait, ActiveValue::Set};
        use temps_database::test_utils::TestDatabase;

        let test_database = match TestDatabase::with_migrations().await {
            Ok(database) => database,
            Err(error)
                if std::env::var_os("TEMPS_TEST_DATABASE_URL").is_none()
                    && temps_database::test_utils::is_container_runtime_unavailable(
                        &error.to_string(),
                    ) =>
            {
                eprintln!("skipping Shield worker test: test database unavailable: {error}");
                return Ok(());
            }
            Err(error) => return Err(error.into()),
        };
        let db = test_database.db.clone();
        let fixtures = crate::test_utils::TestDBMockOperations::new(db.clone()).await?;
        let (project, environment, deployment) = fixtures
            .create_test_project_with_domain("shield.example.test")
            .await?;
        let mut project_row: temps_entities::projects::ActiveModel = project.clone().into();
        project_row.shield_mode = Set("selective".into());
        let project = project_row.update(db.as_ref()).await?;
        let mut environment_row: temps_entities::environments::ActiveModel =
            environment.clone().into();
        environment_row.shield_exempt_paths =
            Set(temps_entities::environments::ShieldExemptPaths(vec![
                "/api/*".into(),
            ]));
        let environment = environment_row.update(db.as_ref()).await?;
        for (name, environment_id, revoked) in [
            ("env", Some(environment.id), false),
            ("project", None, false),
            ("revoked", None, true),
        ] {
            shield_bypass_tokens::ActiveModel {
                project_id: Set(project.id),
                environment_id: Set(environment_id),
                name: Set(name.into()),
                token_hash: Set(format!("{:0>64}", name)),
                revoked_at: Set(revoked.then(chrono::Utc::now)),
                ..Default::default()
            }
            .insert(db.as_ref())
            .await?;
        }

        let (private_key, public_key) = temps_core::worker_ingress::generate_ingress_key()?;
        let token = "node-secret";
        let mut node = test_node(true);
        node.token_hash = hex::encode(<sha2::Sha256 as sha2::Digest>::digest(token));
        node.edge_public_key = Some(public_key);
        nodes::ActiveModel::from(node.clone())
            .insert(db.as_ref())
            .await?;
        let mut internal = test_node(false);
        internal.id = 8;
        internal.name = "worker-internal".into();
        internal.token_hash = node.token_hash.clone();
        nodes::ActiveModel::from(internal)
            .insert(db.as_ref())
            .await?;

        let peer_table = Arc::new(CachedPeerTable::new(db.clone()));
        peer_table.insert_route_for_test(
            "shield.example.test",
            RouteInfo {
                backend: BackendType::Upstream {
                    backends: vec![BackendEntry {
                        address: "192.0.2.7:32000".to_string(),
                        container_id: None,
                        container_name: None,
                    }],
                    round_robin_counter: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                },
                redirect_to: None,
                status_code: None,
                project: Some(Arc::new(project.clone())),
                environment: Some(Arc::new(environment.clone())),
                deployment: Some(Arc::new(deployment)),
                cert_eligible: true,
            },
        );
        let encryption = Arc::new(temps_core::EncryptionService::new(&"11".repeat(32))?);
        let gate = Arc::new(temps_core::RequestPolicyGateSlot::new_default());
        assert!(gate.set(Arc::new(temps_core::OpenRequestPolicyGate)));
        let collected = Arc::new(Collect::default());
        let state = Arc::new(RouteSyncAppState {
            db: db.clone(),
            peer_table: Arc::clone(&peer_table),
            encryption_service: Arc::clone(&encryption),
            request_policy_gate: gate,
            gate: Arc::new(crate::gate::GateService::new(
                db.clone(),
                Arc::clone(&encryption),
            )),
            access_keys: Arc::new(crate::access_keys::AccessKeyService::new(db.clone())),
            shield_events: Arc::new(ShieldEventSinkSlot::fixed(collected.clone())),
        });

        // Route data: a selective environment is pinned for an agent
        // without Shield, and sent with its policy to one with it.
        let without = worker_public_routes(db.as_ref(), &peer_table, false, false)
            .await
            .map_err(|(operation, error)| format!("{operation}: {error}"))?;
        assert!(without.routes.is_empty());
        assert!(
            without.unsupported_reasons[0].contains("Shield mode"),
            "{:?}",
            without.unsupported_reasons
        );
        let with = worker_public_routes(db.as_ref(), &peer_table, false, true)
            .await
            .map_err(|(operation, error)| format!("{operation}: {error}"))?;
        let policy = with.routes[0].shield.clone().expect("a policy");
        assert_eq!(policy.mode, "selective");
        assert_eq!(policy.exempt_paths, ["/api/*"]);
        assert_eq!(
            policy.bypass_token_hashes,
            [format!("{:0>64}", "env"), format!("{:0>64}", "project")]
        );

        // The route snapshot names the stored revision.
        let body = b"compiled snapshot".to_vec();
        let stored = shield_snapshots::ActiveModel {
            sha256: Set(hex::encode(<sha2::Sha256 as sha2::Digest>::digest(&body))),
            body: Set(body.clone()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await?;
        let revision = stored.revision as u64;
        let snapshot = |runs_shield| {
            build_snapshot(
                &state,
                &node,
                1,
                AgentCapabilities {
                    enforces_gate: true,
                    runs_shield,
                    ..AgentCapabilities::default()
                },
            )
        };
        let old = snapshot(false).await?.public_ingress;
        assert!(old.routes.is_empty() && old.shield_routes.is_empty() && old.shield.is_none());
        let new = snapshot(true).await?.public_ingress;
        assert!(new.routes.is_empty());
        assert_eq!(new.shield_routes[0].host, "shield.example.test");
        assert_eq!(
            new.shield,
            Some(temps_core::worker_shield::ShieldRef { revision })
        );

        // The snapshot endpoint.
        let fetch = |token: &str, node_id: i32, revision: u64| {
            get_shield_snapshot(
                State(Arc::clone(&state)),
                bearer(token),
                Path(node_id),
                Query(ShieldSnapshotQuery { revision }),
            )
        };
        assert_eq!(
            fetch("wrong", 7, 0).await.err().map(|e| e.0),
            Some(StatusCode::UNAUTHORIZED)
        );
        assert_eq!(
            fetch(token, 8, 0).await.err().map(|e| e.0),
            Some(StatusCode::FORBIDDEN),
            "a node that takes no public traffic gets no keys"
        );
        let unchanged = fetch(token, 7, revision).await.map_err(|e| e.1)?;
        assert_eq!(unchanged.status(), StatusCode::NOT_MODIFIED);
        let fresh = fetch(token, 7, 0).await.map_err(|e| e.1)?;
        assert_eq!(fresh.status(), StatusCode::OK);
        let fresh: ShieldSnapshotResponse =
            serde_json::from_slice(&axum::body::to_bytes(fresh.into_body(), usize::MAX).await?)?;
        assert_eq!(fresh.revision, revision);
        assert_eq!(BASE64.decode(fresh.body.expect("a body"))?, body);
        let keys = fresh.keys.expect("keys sealed to the node");
        let opened = temps_core::ecies::decrypt_bundle_with_info(
            &private_key,
            &keys.ephemeral_public_key,
            &temps_core::ecies::EncryptedBundle {
                ciphertext: keys.ciphertext,
                nonce: keys.nonce,
            },
            SHIELD_KEYS_INFO,
        )?;
        let material: ShieldKeyMaterial = serde_json::from_slice(&opened)?;
        assert!(!material.keys.is_empty());
        // A stale revision (say, after a restore) gets the newest.
        let stale = fetch(token, 7, revision + 5).await.map_err(|e| e.1)?;
        assert_eq!(stale.status(), StatusCode::OK);

        // The events endpoint.
        let mut mine = event(Some(99));
        mine.project_id = project.id;
        mine.environment_id = environment.id;
        let mut elsewhere = mine.clone();
        elsewhere.environment_id = environment.id + 1000;
        let post = |token: &str, node_id: i32, body: Bytes| {
            post_shield_events(
                State(Arc::clone(&state)),
                bearer(token),
                Path(node_id),
                body,
            )
        };
        let status = |result: Result<_, (StatusCode, String)>| match result {
            Ok(response) => axum::response::IntoResponse::into_response(response).status(),
            Err((status, _)) => status,
        };
        let batch = gzip_json(&vec![mine.clone(), elsewhere]);
        assert_eq!(
            status(post("wrong", 7, batch.clone()).await),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            status(post(token, 8, batch.clone()).await),
            StatusCode::FORBIDDEN
        );
        assert_eq!(status(post(token, 7, batch).await), StatusCode::ACCEPTED);
        {
            let events = collected.events.lock().unwrap();
            assert_eq!(events.len(), 1, "only the environment workers serve");
            assert_eq!(events[0].node_id, Some(7));
        }
        assert_eq!(
            status(
                post(
                    token,
                    7,
                    gzip_json(&vec![mine.clone(); MAX_EVENTS_PER_UPLOAD + 1])
                )
                .await
            ),
            StatusCode::PAYLOAD_TOO_LARGE
        );
        let bomb = gzip_json(&vec![
            "x".repeat(1024);
            MAX_EVENTS_DECOMPRESSED_BYTES / 1024
        ]);
        assert_eq!(
            status(post(token, 7, bomb).await),
            StatusCode::PAYLOAD_TOO_LARGE
        );
        assert_eq!(
            status(post(token, 7, Bytes::from_static(b"[]")).await),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(collected.events.lock().unwrap().len(), 1);
        Ok(())
    }
}
