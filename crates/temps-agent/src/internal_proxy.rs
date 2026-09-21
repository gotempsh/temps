// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Worker HTTP proxy shared by private overlay and opt-in public ingress.
//!
//! Containers on this worker resolve `<env>.<project>.temps.local` to
//! the bridge gateway IP via the per-node Hickory resolver. They open a
//! plain HTTP connection on `:80`. This module is what answers.
//!
//! ## Routing decision
//!
//! Every decision is read from [`crate::route_store::RouteStore`],
//! which the [`crate::route_sync_client::RouteSyncClient`] mirrors
//! from the CP. There is no DB call on the request path; lookup is
//! `HashMap::get` under a brief read lock.
//!
//! ## Backend selection
//!
//! Random pick from the host's healthy backend list. No connection
//! affinity in v1 — most internal traffic is short-lived HTTP and
//! distributing each request independently keeps the math simple. If
//! one worker is down, only ~1/N of requests fail until the next
//! sync round drops it from the list; the client retries and lands on
//! a different backend on the next try.
//!
//! ## Hop-by-hop headers
//!
//! Stripped per RFC 7230 §6.1: `connection`, `keep-alive`,
//! `proxy-authenticate`, `proxy-authorization`, `te`, `trailers`,
//! `transfer-encoding`, `upgrade`. The proxy adds standard
//! `x-forwarded-*` headers and a `x-temps-deployment-id` for log
//! correlation.
//!
//! ## Errors mapped to HTTP
//!
//! - No matching host → `404` with explanatory body.
//! - Host has no healthy backends → `503`.
//! - Upstream connection failure → `502`, retried up to 3 times against
//!   different backends before giving up.
//! - Upstream timeout → `504`.
//!
//! ## Listen scope
//!
//! The private listener binds only to `<bridge_ip>:80`. Public ingress reuses
//! the routing and streaming code through [`public_router`], but its caller
//! identity, host/SNI checks, connection limits, ACME budget, and forwarding
//! metadata come from the explicitly configured public listeners.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use futures::StreamExt;
use parking_lot::RwLock;
use rand::prelude::SliceRandom;
use tokio::net::TcpListener;
use tokio::sync::{Notify, Semaphore};
use tracing::{debug, error, info, warn};

use crate::route_store::{RouteEntry, SharedRouteStore};

#[derive(Clone, Debug)]
pub(crate) struct PublicTlsSni(pub String);

#[derive(Clone)]
pub(crate) struct PublicAcmeConfig {
    pub control_plane_url: String,
    pub node_id: i32,
    pub node_token: String,
}

#[derive(Clone, Copy)]
pub(crate) struct PublicPeer(pub SocketAddr);

#[derive(Clone)]
pub(crate) struct PublicConnectionPermit(pub Arc<tokio::sync::OwnedSemaphorePermit>);

const PUBLIC_TUNNEL_IDLE_TIMEOUT: Duration = Duration::from_secs(300);
const PUBLIC_TUNNEL_WRITE_TIMEOUT: Duration = Duration::from_secs(60);
const ACME_LOOKUP_TIMEOUT: Duration = Duration::from_secs(5);
const ACME_CACHE_CAPACITY: usize = 256;
const ACME_POSITIVE_CACHE_TTL: Duration = Duration::from_secs(60);
const ACME_NEGATIVE_CACHE_TTL: Duration = Duration::from_secs(10);
const ACME_GCRA_PERIOD_MICROS: u64 = 500_000;
const ACME_GCRA_BURST_TOLERANCE_MICROS: u64 = 1_500_000;

/// Hop-by-hop headers per RFC 7230 §6.1. Lowercased so comparison is
/// trivial (axum normalises, but be explicit).
const HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailers",
    "transfer-encoding",
    "upgrade",
    "host",
    "forwarded",
    "x-forwarded-for",
    "x-forwarded-host",
    "x-forwarded-proto",
    "x-real-ip",
    "x-temps-deployment-id",
];

/// Per-attempt upstream timeout. Internal-zone calls are between
/// containers on the same overlay; if 30s isn't enough something is
/// already wrong elsewhere.
const UPSTREAM_TIMEOUT: Duration = Duration::from_secs(30);
const PUBLIC_UPSTREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// Maximum upstream retry attempts (across distinct backends) on
/// connect failure before returning 502.
const MAX_RETRIES: usize = 3;

/// Caller identity is derived from Docker's current network attachments. Keep
/// the acceptance window short so a recycled bridge IP cannot retain the
/// previous container's project identity.
const IDENTITY_REFRESH_INTERVAL: Duration = Duration::from_secs(2);
const IDENTITY_MAX_AGE: Duration = Duration::from_secs(3);
const IDENTITY_INSPECTION_PROJECT_BURST: usize = 16;
const IDENTITY_INSPECTION_PROJECT_REFILL: usize = 8;
const IDENTITY_INSPECTION_REFILL_INTERVAL: Duration = Duration::from_millis(125);
const IDENTITY_INSPECTION_PROJECT_CONCURRENCY: usize = 2;
const IDENTITY_INSPECTION_GLOBAL_CONCURRENCY: usize = 32;
const IDENTITY_INSPECTION_QUEUE_TIMEOUT: Duration = Duration::from_millis(100);
const IDENTITY_INSPECTION_TIMEOUT: Duration = Duration::from_millis(500);

struct CallerIdentitySnapshot {
    identities: HashMap<IpAddr, String>,
    refreshed_at: Instant,
}

impl CallerIdentitySnapshot {
    fn new(identities: HashMap<IpAddr, String>) -> Self {
        Self {
            identities,
            refreshed_at: Instant::now(),
        }
    }

    fn clear(&mut self) {
        self.identities.clear();
    }
}

struct InspectionQuota {
    tokens: Arc<Semaphore>,
    concurrency: Arc<Semaphore>,
}

impl InspectionQuota {
    fn new() -> Self {
        Self {
            tokens: Arc::new(Semaphore::new(IDENTITY_INSPECTION_PROJECT_BURST)),
            concurrency: Arc::new(Semaphore::new(IDENTITY_INSPECTION_PROJECT_CONCURRENCY)),
        }
    }
}

/// Shared HTTP client + route store for the proxy's request handler.
struct ProxyState {
    client: reqwest::Client,
    store: SharedRouteStore,
    docker: Option<bollard::Docker>,
    inspection_quotas: Arc<RwLock<HashMap<i32, Arc<InspectionQuota>>>>,
    inspection_concurrency: Arc<Semaphore>,
    caller_identities: Arc<RwLock<CallerIdentitySnapshot>>,
    public_mode: bool,
    forwarded_proto: &'static str,
    acme: Option<PublicAcmeConfig>,
    acme_relay: Arc<AcmeRelayState>,
}

#[derive(Clone)]
struct AcmeCacheEntry {
    value: Option<String>,
    expires_at: Instant,
}

struct AcmeRelayState {
    started_at: Instant,
    theoretical_arrival_micros: AtomicU64,
    cache: arc_swap::ArcSwap<HashMap<String, AcmeCacheEntry>>,
    concurrency: Semaphore,
}

impl AcmeRelayState {
    fn new() -> Self {
        Self {
            started_at: Instant::now(),
            theoretical_arrival_micros: AtomicU64::new(0),
            cache: arc_swap::ArcSwap::from_pointee(HashMap::new()),
            concurrency: Semaphore::new(2),
        }
    }

    fn try_take(&self) -> bool {
        self.try_take_at(duration_micros(self.started_at.elapsed()))
    }

    fn try_take_at(&self, now_micros: u64) -> bool {
        loop {
            let current = self.theoretical_arrival_micros.load(Ordering::Acquire);
            if current > now_micros.saturating_add(ACME_GCRA_BURST_TOLERANCE_MICROS) {
                return false;
            }
            let next = current
                .max(now_micros)
                .saturating_add(ACME_GCRA_PERIOD_MICROS);
            if self
                .theoretical_arrival_micros
                .compare_exchange_weak(current, next, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return true;
            }
        }
    }
}

fn duration_micros(duration: Duration) -> u64 {
    duration.as_micros().min(u128::from(u64::MAX)) as u64
}

/// Spawn the proxy on `<bridge_ip>:80`. Returns once the listener is
/// bound; the request loop runs in a background task and exits when
/// `shutdown` is notified.
///
/// Idempotent at the call site: callers should only invoke this once
/// per agent process.
pub async fn spawn(
    bridge_ip: IpAddr,
    port: u16,
    store: SharedRouteStore,
    docker: bollard::Docker,
    shutdown: Arc<Notify>,
) -> std::io::Result<()> {
    let addr = SocketAddr::new(bridge_ip, port);
    let listener = TcpListener::bind(addr).await?;
    info!(%addr, "internal edge proxy listening");

    let client = reqwest::Client::builder()
        .timeout(UPSTREAM_TIMEOUT)
        // No redirect-following: internal traffic should be
        // self-contained, and following a redirect to a public URL
        // from inside the overlay is a foot-gun.
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| std::io::Error::other(e.to_string()))?;

    let initial_identities = load_source_identities(&docker)
        .await
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    let caller_identities = Arc::new(RwLock::new(CallerIdentitySnapshot::new(initial_identities)));
    let inspection_quotas = Arc::new(RwLock::new(HashMap::new()));
    let state = Arc::new(ProxyState {
        client,
        store,
        docker: Some(docker.clone()),
        inspection_quotas: Arc::clone(&inspection_quotas),
        inspection_concurrency: Arc::new(Semaphore::new(IDENTITY_INSPECTION_GLOBAL_CONCURRENCY)),
        caller_identities: Arc::clone(&caller_identities),
        public_mode: false,
        forwarded_proto: "http",
        acme: None,
        acme_relay: Arc::new(AcmeRelayState::new()),
    });
    let app = axum::Router::new().fallback(handle).with_state(state);

    let identity_shutdown = Arc::clone(&shutdown);
    tokio::spawn(async move {
        loop {
            let options = bollard::query_parameters::EventsOptionsBuilder::new().build();
            let mut events = docker.events(Some(options));
            let mut interval = tokio::time::interval(IDENTITY_REFRESH_INTERVAL);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            let reconnect = loop {
                tokio::select! {
                    _ = identity_shutdown.notified() => return,
                    _ = interval.tick() => {
                        refresh_source_identities(&docker, &caller_identities).await;
                    }
                    event = events.next() => match event {
                        Some(Ok(event)) if identity_event_requires_refresh(&event) => {
                            // Invalidate before reloading so an IP released by
                            // this event cannot retain its old project identity.
                            caller_identities.write().clear();
                            refresh_source_identities(&docker, &caller_identities).await;
                        }
                        Some(Ok(_)) => {}
                        Some(Err(error)) => {
                            caller_identities.write().clear();
                            warn!(%error, "Docker event stream failed; denying workload traffic until reconnect");
                            break true;
                        }
                        None => {
                            caller_identities.write().clear();
                            warn!("Docker event stream ended; denying workload traffic until reconnect");
                            break true;
                        }
                    }
                }
            };

            if reconnect {
                tokio::select! {
                    _ = identity_shutdown.notified() => return,
                    _ = tokio::time::sleep(Duration::from_secs(1)) => {}
                }
            }
        }
    });

    let budget_shutdown = Arc::clone(&shutdown);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(IDENTITY_INSPECTION_REFILL_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = budget_shutdown.notified() => return,
                _ = interval.tick() => {
                    let quotas = inspection_quotas.read().values().cloned().collect::<Vec<_>>();
                    for quota in quotas {
                        let missing = IDENTITY_INSPECTION_PROJECT_BURST
                            .saturating_sub(quota.tokens.available_permits());
                        quota.tokens.add_permits(
                            missing.min(IDENTITY_INSPECTION_PROJECT_REFILL),
                        );
                    }
                }
            }
        }
    });

    tokio::spawn(async move {
        let server = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(async move {
            shutdown.notified().await;
            info!("internal edge proxy shutting down");
        });
        if let Err(e) = server.await {
            error!(error = %e, "internal edge proxy exited with error");
        }
    });
    Ok(())
}

async fn handle(State(state): State<Arc<ProxyState>>, req: Request) -> Response {
    let host = match extract_host(req.headers()) {
        Some(h) => h,
        None => {
            return error_body(StatusCode::BAD_REQUEST, "missing or malformed Host header");
        }
    };

    if state.public_mode {
        if state.forwarded_proto == "https"
            && req
                .extensions()
                .get::<PublicTlsSni>()
                .map(|sni| sni.0.as_str())
                != Some(host.as_str())
        {
            return error_body(
                StatusCode::MISDIRECTED_REQUEST,
                "TLS server name does not match Host",
            );
        }
        if state.forwarded_proto == "http"
            && req.uri().path().starts_with("/.well-known/acme-challenge/")
        {
            return proxy_acme_challenge(&state, &host, req).await;
        }
        let entry = match state.store.lookup_public(&host) {
            Some(entry) => entry,
            None => return error_body(StatusCode::NOT_FOUND, "no public route for this host"),
        };
        if entry.backends.is_empty() {
            return error_body(StatusCode::SERVICE_UNAVAILABLE, "no live backends");
        }
        if is_upgrade_request(req.headers()) {
            return proxy_upgrade(&entry, req, state.forwarded_proto).await;
        }
        return proxy_with_retries(&state, &entry, req).await;
    }

    // Reject non-temps.local up front. The proxy is not a generic
    // forwarder; matching only our internal zone makes accidental
    // misconfiguration (e.g. someone CNAME'd a public name to our
    // bridge) fail loudly instead of forwarding traffic somewhere it
    // shouldn't go.
    if !host.ends_with(".temps.local") {
        return error_body(
            StatusCode::NOT_FOUND,
            "internal proxy serves *.temps.local only",
        );
    }

    let entry = match state.store.lookup(&host) {
        Some(e) => e,
        None => {
            warn!(%host, "no route in local store");
            return error_body(StatusCode::NOT_FOUND, "no internal route for this host");
        }
    };

    if !caller_may_access_route(&state, req.extensions(), &entry).await {
        warn!(%host, route_project_id = ?entry.project_id, "blocked cross-project internal proxy request");
        return error_body(
            StatusCode::FORBIDDEN,
            "internal route is not accessible from this workload",
        );
    }

    if entry.backends.is_empty() {
        warn!(%host, "host has no backends");
        return error_body(StatusCode::SERVICE_UNAVAILABLE, "no live backends");
    }

    // WebSocket / generic protocol upgrade. Reqwest's HTTP client
    // doesn't tunnel after a 101 response — it returns the response
    // headers and considers the request done — so we have to take the
    // upgrade off the wire ourselves: open a raw TCP connection to a
    // backend, replay the request line + headers exactly, then bridge
    // bytes between the upgraded client socket and that backend
    // socket. Works for `Upgrade: websocket`, but also for any other
    // RFC 7230 Upgrade target (HTTP/2 prior knowledge isn't supported
    // here — internal traffic is plain HTTP/1.1).
    if is_upgrade_request(req.headers()) {
        return proxy_upgrade(&entry, req, state.forwarded_proto).await;
    }

    proxy_with_retries(&state, &entry, req).await
}

/// Bind the public HTTP listener, accepting port zero for integration tests.
/// Only exact hosts present in the authenticated public snapshot are served.
pub async fn spawn_public_http(
    address: SocketAddr,
    store: SharedRouteStore,
    shutdown: Arc<Notify>,
) -> std::io::Result<SocketAddr> {
    let listener = TcpListener::bind(address).await?;
    let bound = listener.local_addr()?;
    let app = public_router(store, "http", None)?;
    tokio::spawn(async move {
        let server = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(async move {
            shutdown.notified().await;
        });
        if let Err(error) = server.await {
            error!(%error, "public worker HTTP ingress exited");
        }
    });
    Ok(bound)
}

pub(crate) fn public_router(
    store: SharedRouteStore,
    forwarded_proto: &'static str,
    acme: Option<PublicAcmeConfig>,
) -> std::io::Result<axum::Router> {
    let client = reqwest::Client::builder()
        .connect_timeout(UPSTREAM_TIMEOUT)
        // Reqwest applies this between successful reads, rather than to the
        // whole response. Active uploads, SSE responses, and streamed bodies
        // may therefore live indefinitely while stalled upstreams are bounded.
        .read_timeout(PUBLIC_UPSTREAM_IDLE_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    let state = Arc::new(ProxyState {
        client,
        store,
        docker: None,
        inspection_quotas: Arc::new(RwLock::new(HashMap::new())),
        inspection_concurrency: Arc::new(Semaphore::new(1)),
        caller_identities: Arc::new(RwLock::new(CallerIdentitySnapshot::new(HashMap::new()))),
        public_mode: true,
        forwarded_proto,
        acme,
        acme_relay: Arc::new(AcmeRelayState::new()),
    });
    Ok(axum::Router::new().fallback(handle).with_state(state))
}

async fn proxy_acme_challenge(state: &ProxyState, host: &str, req: Request) -> Response {
    if !matches!(
        *req.method(),
        axum::http::Method::GET | axum::http::Method::HEAD
    ) || {
        let (enabled, authorized, _) = state.store.public_ingress_runtime_status();
        !enabled || !authorized
    } {
        return error_body(StatusCode::NOT_FOUND, "ACME challenge not available");
    }
    let token = req
        .uri()
        .path()
        .trim_start_matches("/.well-known/acme-challenge/");
    if token.is_empty()
        || token.len() > 512
        || !token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return error_body(StatusCode::BAD_REQUEST, "invalid ACME challenge token");
    }
    let Some(acme) = state.acme.as_ref() else {
        return error_body(
            StatusCode::SERVICE_UNAVAILABLE,
            "ACME challenge origin unavailable",
        );
    };
    let cache_key = format!("{host}\0{token}");
    let cache = state.acme_relay.cache.load();
    if let Some(entry) = cache.get(&cache_key) {
        if entry.expires_at > Instant::now() {
            return match &entry.value {
                Some(value) => Response::new(Body::from(value.clone())),
                None => error_body(StatusCode::NOT_FOUND, "ACME challenge not available"),
            };
        }
    }
    drop(cache);
    if !state.acme_relay.try_take() {
        return acme_rate_limited();
    }
    let Ok(_lookup_permit) = state.acme_relay.concurrency.try_acquire() else {
        return acme_rate_limited();
    };
    let url = format!(
        "{}/api/internal/nodes/{}/acme-challenge",
        acme.control_plane_url.trim_end_matches('/'),
        acme.node_id,
    );
    let lookup = async {
        let upstream = state
            .client
            .get(url)
            .bearer_auth(&acme.node_token)
            .query(&[("host", host), ("token", token)])
            .send()
            .await
            .map_err(|_| ())?;
        let status = upstream.status();
        let mut stream = upstream.bytes_stream();
        let mut body = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| ())?;
            if body.len().saturating_add(chunk.len()) > 16 * 1024 {
                return Err(());
            }
            body.extend_from_slice(&chunk);
        }
        Ok::<_, ()>((status, body))
    };
    let Ok(Ok((upstream_status, body))) = tokio::time::timeout(ACME_LOOKUP_TIMEOUT, lookup).await
    else {
        return error_body(StatusCode::BAD_GATEWAY, "ACME challenge origin unavailable");
    };
    if upstream_status != reqwest::StatusCode::OK {
        cache_acme_result(state, cache_key, None, ACME_NEGATIVE_CACHE_TTL);
        return error_body(StatusCode::NOT_FOUND, "ACME challenge not available");
    }
    #[derive(serde::Deserialize)]
    struct ChallengeResponse {
        key_authorization: String,
    }
    match serde_json::from_slice::<ChallengeResponse>(&body) {
        Ok(challenge) if challenge.key_authorization.len() <= 4096 => {
            cache_acme_result(
                state,
                cache_key,
                Some(challenge.key_authorization.clone()),
                ACME_POSITIVE_CACHE_TTL,
            );
            Response::new(Body::from(challenge.key_authorization))
        }
        _ => error_body(StatusCode::BAD_GATEWAY, "ACME challenge origin unavailable"),
    }
}

fn acme_rate_limited() -> Response {
    let mut response = error_body(StatusCode::TOO_MANY_REQUESTS, "ACME lookup rate limited");
    response
        .headers_mut()
        .insert("retry-after", HeaderValue::from_static("1"));
    response
}

fn cache_acme_result(state: &ProxyState, key: String, value: Option<String>, ttl: Duration) {
    let expires_at = Instant::now() + ttl;
    state.acme_relay.cache.rcu(|current| {
        let mut cache = (**current).clone();
        let now = Instant::now();
        cache.retain(|_, entry| entry.expires_at > now);
        if cache.len() >= ACME_CACHE_CAPACITY {
            if let Some(oldest) = cache
                .iter()
                .min_by_key(|(_, entry)| entry.expires_at)
                .map(|(key, _)| key.clone())
            {
                cache.remove(&oldest);
            }
        }
        cache.insert(
            key.clone(),
            AcmeCacheEntry {
                value: value.clone(),
                expires_at,
            },
        );
        Arc::new(cache)
    });
}

async fn caller_may_access_route(
    state: &ProxyState,
    extensions: &axum::http::Extensions,
    entry: &RouteEntry,
) -> bool {
    let Some(route_project_id) = entry.project_id else {
        // Legacy or malformed snapshots without project metadata are not
        // safe to expose through the workload-to-workload proxy.
        return false;
    };
    let Some(peer) = extensions.get::<ConnectInfo<SocketAddr>>().map(|c| c.0) else {
        return false;
    };
    let Some(container_id) = candidate_container_id(&state.caller_identities, peer.ip()) else {
        return false;
    };
    // Reject cross-project callers from memory before they can consume any
    // Docker inspection capacity.
    if !state
        .store
        .container_id_has_project(&container_id, route_project_id)
    {
        return false;
    }
    if !source_container_is_current(state, peer.ip(), &container_id, route_project_id).await {
        return false;
    }
    true
}

/// Verify the cached candidate against Docker's current running-container and
/// network state. A bounded token bucket prevents workload traffic from
/// amplifying without limit into Docker-daemon API calls; exhausted requests
/// fail closed.
async fn source_container_is_current(
    state: &ProxyState,
    source_ip: IpAddr,
    container_id: &str,
    project_id: i32,
) -> bool {
    use bollard::query_parameters::InspectContainerOptions;

    let quota = inspection_quota(state, project_id);
    let project_slot = Arc::clone(&quota.concurrency).try_acquire_owned().ok();
    let Some(_project_slot) = project_slot else {
        return false;
    };
    let token = Arc::clone(&quota.tokens).try_acquire_owned().ok();
    let Some(token) = token else {
        return false;
    };
    token.forget();

    let global_slot = tokio::time::timeout(
        IDENTITY_INSPECTION_QUEUE_TIMEOUT,
        Arc::clone(&state.inspection_concurrency).acquire_owned(),
    )
    .await;
    let Ok(Ok(_global_slot)) = global_slot else {
        return false;
    };

    let Some(docker) = state.docker.as_ref() else {
        return false;
    };
    let inspected = tokio::time::timeout(
        IDENTITY_INSPECTION_TIMEOUT,
        docker.inspect_container(container_id, None::<InspectContainerOptions>),
    )
    .await;
    let Ok(Ok(inspected)) = inspected else {
        return false;
    };
    if inspected.state.and_then(|container| container.running) != Some(true) {
        return false;
    }
    let Some(networks) = inspected
        .network_settings
        .and_then(|settings| settings.networks)
    else {
        return false;
    };
    let owns_source_ip = networks.values().any(|endpoint| {
        [
            endpoint.ip_address.as_deref(),
            endpoint.global_ipv6_address.as_deref(),
        ]
        .into_iter()
        .flatten()
        .filter_map(|address| address.parse::<IpAddr>().ok())
        .any(|address| address == source_ip)
    });
    owns_source_ip
}

fn inspection_quota(state: &ProxyState, project_id: i32) -> Arc<InspectionQuota> {
    if let Some(quota) = state.inspection_quotas.read().get(&project_id).cloned() {
        return quota;
    }
    state
        .inspection_quotas
        .write()
        .entry(project_id)
        .or_insert_with(|| Arc::new(InspectionQuota::new()))
        .clone()
}

/// Resolve the peer address through the local Docker daemon. The daemon's
/// network attachment state is the authority for which container owns an IP;
/// route destination addresses are not caller identity and may be hostnames or
/// shared node addresses.
fn candidate_container_id(
    caller_identities: &RwLock<CallerIdentitySnapshot>,
    source_ip: IpAddr,
) -> Option<String> {
    let snapshot = caller_identities.read();
    if snapshot.refreshed_at.elapsed() > IDENTITY_MAX_AGE {
        return None;
    }
    snapshot.identities.get(&source_ip).cloned()
}

fn identity_event_requires_refresh(event: &bollard::models::EventMessage) -> bool {
    use bollard::models::EventMessageTypeEnum;

    let identity_action = matches!(
        event.action.as_deref(),
        Some("start" | "stop" | "die" | "kill" | "destroy" | "connect" | "disconnect")
    );
    identity_action
        && matches!(
            event.typ,
            Some(EventMessageTypeEnum::CONTAINER | EventMessageTypeEnum::NETWORK)
        )
}

async fn refresh_source_identities(
    docker: &bollard::Docker,
    caller_identities: &RwLock<CallerIdentitySnapshot>,
) {
    match load_source_identities(docker).await {
        Ok(snapshot) => {
            *caller_identities.write() = CallerIdentitySnapshot::new(snapshot);
        }
        Err(error) => {
            caller_identities.write().clear();
            warn!(%error, "failed to refresh internal proxy caller identities; denying workload traffic");
        }
    }
}

async fn load_source_identities(
    docker: &bollard::Docker,
) -> Result<HashMap<IpAddr, String>, bollard::errors::Error> {
    use bollard::query_parameters::ListContainersOptions;

    let containers = docker
        .list_containers(Some(ListContainersOptions {
            all: false,
            ..Default::default()
        }))
        .await?;

    let mut identities = HashMap::new();
    for container in containers {
        let Some(container_id) = container.id else {
            continue;
        };
        let Some(networks) = container
            .network_settings
            .as_ref()
            .and_then(|settings| settings.networks.as_ref())
        else {
            continue;
        };
        for endpoint in networks.values() {
            for address in [
                endpoint.ip_address.as_deref(),
                endpoint.global_ipv6_address.as_deref(),
            ]
            .into_iter()
            .flatten()
            {
                if let Ok(ip) = address.parse::<IpAddr>() {
                    identities.insert(ip, container_id.clone());
                }
            }
        }
    }
    Ok(identities)
}

/// True if the client requested a protocol upgrade. We check both the
/// `connection: upgrade` token (per RFC 7230 §6.7) and the presence of
/// `upgrade:` — the websocket case must satisfy both.
fn is_upgrade_request(headers: &HeaderMap) -> bool {
    let conn = headers
        .get("connection")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    let has_upgrade_token = conn.split(',').any(|t| t.trim() == "upgrade");
    has_upgrade_token && headers.contains_key("upgrade")
}

/// Tunnel an HTTP/1.1 Upgrade through to a backend. Picks one backend
/// at random (no retries — upgraded connections are unique to a
/// specific backend, retrying after a partial handshake would mean
/// duplicating client bytes). Forwards the original request line and
/// headers (minus `Host`, which we override so the backend sees the
/// real downstream host), reads the backend's status line + headers
/// up to the empty line, and pipes bytes bidirectionally until either
/// side closes.
async fn proxy_upgrade(
    entry: &RouteEntry,
    req: Request,
    forwarded_proto: &'static str,
) -> Response {
    use rand::prelude::IndexedRandom;
    let backend = {
        let mut rng = rand::rng();
        entry.backends.choose(&mut rng).cloned()
    };
    let Some(backend) = backend else {
        return error_body(StatusCode::SERVICE_UNAVAILABLE, "no live backends");
    };

    let method = req.method().clone();
    let uri = req.uri().clone();
    let path_and_query = uri
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or("/")
        .to_string();
    let headers = req.headers().clone();
    let public_peer = req.extensions().get::<PublicPeer>().copied().or_else(|| {
        req.extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .map(|peer| PublicPeer(peer.0))
    });
    let connection_permit = req
        .extensions()
        .get::<PublicConnectionPermit>()
        .map(|permit| Arc::clone(&permit.0));
    let original_host = headers
        .get("host")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    // Dial backend up front. If this fails the client sees a 502
    // before its own upgrade attempt is committed, which is the
    // friendly outcome.
    let mut backend_sock = match tokio::time::timeout(
        UPSTREAM_TIMEOUT,
        tokio::net::TcpStream::connect(&backend.address),
    )
    .await
    {
        Ok(Ok(socket)) => socket,
        Ok(Err(e)) => {
            warn!(backend = %backend.address, error = %e, "ws upgrade backend connect failed");
            return error_body(StatusCode::BAD_GATEWAY, "application backend unavailable");
        }
        Err(_) => {
            return error_body(
                StatusCode::GATEWAY_TIMEOUT,
                "application backend unavailable",
            )
        }
    };

    // Build the request as raw bytes. Manually so we don't have to
    // wrestle with hyper's typed encoder for this narrow path.
    let mut wire = format!("{} {} HTTP/1.1\r\n", method.as_str(), path_and_query).into_bytes();
    for (name, value) in headers.iter() {
        let name_str = name.as_str();
        // Drop hop-by-hop except `connection` and `upgrade`, which the
        // upgrade flow needs preserved verbatim per RFC 6455 §4.
        if matches!(
            name_str,
            "proxy-authenticate" | "proxy-authorization" | "te" | "trailers" | "transfer-encoding"
        ) {
            continue;
        }
        // Strip client-supplied forwarding/identity headers (ADR-020 WS-3 /
        // netiso-5). The proxy sets `x-forwarded-*` and `x-temps-deployment-id`
        // authoritatively below; passing inbound copies through would let any
        // overlay client spoof its source host/proto or impersonate another
        // deployment to the backend.
        if name_str == "forwarded"
            || name_str.starts_with("x-forwarded-")
            || name_str.starts_with("x-temps-")
            || name_str == "x-real-ip"
        {
            continue;
        }
        if name_str == "host" {
            continue;
        }
        wire.extend_from_slice(name_str.as_bytes());
        wire.extend_from_slice(b": ");
        wire.extend_from_slice(value.as_bytes());
        wire.extend_from_slice(b"\r\n");
    }
    if let Some(h) = &original_host {
        wire.extend_from_slice(format!("host: {}\r\n", h).as_bytes());
    }
    if let Some(deployment_id) = entry.deployment_id {
        wire.extend_from_slice(format!("x-temps-deployment-id: {}\r\n", deployment_id).as_bytes());
    }
    if let Some(h) = &original_host {
        wire.extend_from_slice(format!("x-forwarded-host: {}\r\n", h).as_bytes());
    }
    wire.extend_from_slice(format!("x-forwarded-proto: {forwarded_proto}\r\n\r\n").as_bytes());
    if let Some(peer) = public_peer {
        let forwarding = format!(
            "x-forwarded-for: {}\r\nx-real-ip: {}\r\n",
            peer.0.ip(),
            peer.0.ip()
        );
        let insert_at = wire.len().saturating_sub(2);
        wire.splice(insert_at..insert_at, forwarding.bytes());
    }

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    match tokio::time::timeout(UPSTREAM_TIMEOUT, backend_sock.write_all(&wire)).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            warn!(backend = %backend.address, error = %e, "ws backend write request failed");
            return error_body(StatusCode::BAD_GATEWAY, "upstream write failed");
        }
        Err(_) => {
            return error_body(
                StatusCode::GATEWAY_TIMEOUT,
                "application backend unavailable",
            )
        }
    }

    // Read the backend's response headers. We only need to peek at
    // the status line — anything in the 100s/200s with the upgrade
    // sequence completes the tunnel; anything else we relay verbatim
    // and stop. We hand-parse to avoid pulling in another HTTP
    // parser; the response shape is "STATUS\r\nHEADER\r\n...\r\n\r\n".
    let mut buf = Vec::with_capacity(4096);
    let mut tmp = [0u8; 1024];
    let header_end = loop {
        match tokio::time::timeout(UPSTREAM_TIMEOUT, backend_sock.read(&mut tmp)).await {
            Ok(Ok(0)) => {
                warn!(backend = %backend.address, "backend closed before headers");
                return error_body(StatusCode::BAD_GATEWAY, "upstream closed early");
            }
            Ok(Ok(n)) => {
                buf.extend_from_slice(&tmp[..n]);
                if let Some(idx) = find_header_end(&buf) {
                    break idx;
                }
                if buf.len() > 64 * 1024 {
                    warn!("ws backend headers exceed 64KiB; aborting");
                    return error_body(StatusCode::BAD_GATEWAY, "upstream header too large");
                }
            }
            Ok(Err(e)) => {
                warn!(backend = %backend.address, error = %e, "ws backend read failed");
                return error_body(StatusCode::BAD_GATEWAY, "upstream read failed");
            }
            Err(_) => {
                return error_body(
                    StatusCode::GATEWAY_TIMEOUT,
                    "application backend unavailable",
                )
            }
        }
    };
    let header_bytes = buf[..header_end].to_vec();
    let leftover = buf[header_end..].to_vec();

    // Parse status code + parse the header block back into typed
    // headers we can hand to axum.
    let (status, headers_out) = match parse_response_head(&header_bytes) {
        Ok(v) => v,
        Err(e) => {
            warn!(error = %e, "ws backend response parse failed");
            return error_body(StatusCode::BAD_GATEWAY, "upstream malformed");
        }
    };

    // Non-101 ⇒ backend declined the upgrade; relay status+headers
    // and any leftover body, then close. No bidirectional tunnel.
    if status != StatusCode::SWITCHING_PROTOCOLS {
        let mut resp = Response::new(Body::from(leftover));
        *resp.status_mut() = status;
        for (name, value) in headers_out.iter() {
            if HOP_BY_HOP.contains(&name.as_str()) {
                continue;
            }
            resp.headers_mut().insert(name.clone(), value.clone());
        }
        return resp;
    }

    // 101: upgrade the *client* connection so axum hands us the raw
    // socket, then bridge bytes both ways. The `on_upgrade` future
    // resolves once axum's response (the 101 we're about to build)
    // has been written.
    let on_upgrade = hyper::upgrade::on(req);
    let response_bytes = headers_out;
    // Build the 101 response we send to the client. Mirror the
    // backend's headers verbatim — Sec-WebSocket-Accept comes from
    // there and must round-trip unchanged.
    let mut client_resp = Response::new(Body::empty());
    *client_resp.status_mut() = StatusCode::SWITCHING_PROTOCOLS;
    for (name, value) in response_bytes.iter() {
        client_resp
            .headers_mut()
            .insert(name.clone(), value.clone());
    }

    // Spawn the bidirectional copy after the response is sent. We
    // need to detach the futures since axum returns the response
    // before the tunnel starts.
    tokio::spawn(async move {
        // Keep the listener's bounded connection slot owned until the
        // detached upgraded tunnel actually exits.
        let _connection_permit = connection_permit;
        let upgraded = match tokio::time::timeout(UPSTREAM_TIMEOUT, on_upgrade).await {
            Ok(Ok(u)) => u,
            Ok(Err(e)) => {
                warn!(error = %e, "ws client upgrade failed");
                return;
            }
            Err(_) => {
                warn!("ws client upgrade timed out");
                return;
            }
        };
        // hyper::upgrade::Upgraded uses hyper's I/O traits, not
        // tokio's. Wrap with TokioIo so `copy_bidirectional` and
        // `write_all` work.
        let mut client_sock = hyper_util::rt::TokioIo::new(upgraded);
        // Replay any leftover bytes the backend already sent past the
        // header terminator (ws frames often arrive in the same
        // packet as the 101 from the upstream).
        if !leftover.is_empty() {
            if let Err(e) = tokio::time::timeout(
                PUBLIC_TUNNEL_WRITE_TIMEOUT,
                client_sock.write_all(&leftover),
            )
            .await
            .map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::TimedOut, "leftover write timed out")
            })
            .and_then(|result| result)
            {
                warn!(error = %e, "ws leftover write to client failed");
                return;
            }
        }
        let result =
            copy_tunnel_with_idle(client_sock, backend_sock, PUBLIC_TUNNEL_IDLE_TIMEOUT).await;
        match result {
            Ok((up, down)) => {
                debug!(up_bytes = up, down_bytes = down, "ws tunnel closed");
            }
            Err(e) => {
                debug!(error = %e, "ws tunnel ended with error");
            }
        }
    });

    client_resp
}

async fn copy_tunnel_with_idle<C, B>(
    client: C,
    backend: B,
    idle_timeout: Duration,
) -> std::io::Result<(u64, u64)>
where
    C: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    B: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let (client_read, client_write) = tokio::io::split(client);
    let (backend_read, backend_write) = tokio::io::split(backend);
    let activity_origin = Instant::now();
    let last_activity_micros = Arc::new(AtomicU64::new(0));
    let tunnel = async {
        tokio::try_join!(
            copy_tunnel_direction(
                client_read,
                backend_write,
                activity_origin,
                Arc::clone(&last_activity_micros)
            ),
            copy_tunnel_direction(
                backend_read,
                client_write,
                activity_origin,
                Arc::clone(&last_activity_micros)
            )
        )
    };
    tokio::pin!(tunnel);
    let result = loop {
        let observed = last_activity_micros.load(Ordering::Acquire);
        let deadline = activity_origin + Duration::from_micros(observed) + idle_timeout;
        tokio::select! {
            result = &mut tunnel => break result,
            _ = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => {
                let elapsed_since_observed = activity_origin
                    .elapsed()
                    .saturating_sub(Duration::from_micros(observed));
                if elapsed_since_observed >= idle_timeout
                    && last_activity_micros.load(Ordering::Acquire) == observed
                {
                    break Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "tunnel idle timeout",
                    ));
                }
            }
        }
    };
    result
}

async fn copy_tunnel_direction<R, W>(
    mut reader: R,
    mut writer: W,
    activity_origin: Instant,
    last_activity_micros: Arc<AtomicU64>,
) -> std::io::Result<u64>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut copied = 0u64;
    let mut buffer = [0u8; 16 * 1024];
    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            tokio::time::timeout(PUBLIC_TUNNEL_WRITE_TIMEOUT, writer.shutdown())
                .await
                .map_err(|_| {
                    std::io::Error::new(std::io::ErrorKind::TimedOut, "tunnel shutdown timeout")
                })??;
            return Ok(copied);
        }
        tokio::time::timeout(
            PUBLIC_TUNNEL_WRITE_TIMEOUT,
            writer.write_all(&buffer[..read]),
        )
        .await
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "tunnel write timeout"))??;
        last_activity_micros.fetch_max(
            duration_micros(activity_origin.elapsed()),
            Ordering::Release,
        );
        copied = copied.saturating_add(read as u64);
    }
}

/// Find the position immediately after the first `\r\n\r\n` in `buf`.
/// Returns `None` if the terminator isn't yet present.
fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4)
}

/// Parse a raw HTTP response head (status line + headers) into a
/// typed `(StatusCode, HeaderMap)`. Strict about CRLF — internal
/// servers all conform.
fn parse_response_head(buf: &[u8]) -> Result<(StatusCode, HeaderMap), String> {
    let text = std::str::from_utf8(buf).map_err(|e| format!("non-utf8: {e}"))?;
    let mut lines = text.split("\r\n");
    let status_line = lines.next().ok_or("empty response")?;
    // "HTTP/1.1 101 Switching Protocols"
    let mut parts = status_line.splitn(3, ' ');
    let _version = parts.next().ok_or("missing version")?;
    let code_str = parts.next().ok_or("missing code")?;
    let code: u16 = code_str.parse().map_err(|e| format!("bad code: {e}"))?;
    let status = StatusCode::from_u16(code).map_err(|e| format!("bad status: {e}"))?;

    let mut headers = HeaderMap::new();
    for line in lines {
        if line.is_empty() {
            break;
        }
        let (name, value) = line.split_once(':').ok_or("malformed header")?;
        let name_trim = name.trim();
        let value_trim = value.trim();
        if name_trim.is_empty() {
            continue;
        }
        let name = HeaderName::from_bytes(name_trim.as_bytes())
            .map_err(|e| format!("bad header name {name_trim:?}: {e}"))?;
        let value =
            HeaderValue::from_str(value_trim).map_err(|e| format!("bad header value: {e}"))?;
        headers.append(name, value);
    }
    Ok((status, headers))
}

async fn proxy_with_retries(state: &ProxyState, entry: &RouteEntry, req: Request) -> Response {
    // Snapshot method/uri/headers up front; we may need to clone
    // body bytes if we want true retries on body-bearing requests.
    // For v1 we only retry idempotent methods (GET/HEAD/OPTIONS) so
    // the body issue is moot. Methods with bodies get one shot.
    let method = req.method().clone();
    let uri = req.uri().clone();
    let headers = req.headers().clone();
    let public_peer = req.extensions().get::<PublicPeer>().copied().or_else(|| {
        req.extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .map(|peer| PublicPeer(peer.0))
    });
    let metadata = ForwardRequestMetadata {
        method,
        uri,
        headers,
        public_peer,
    };
    let retryable = metadata.method == axum::http::Method::GET
        || metadata.method == axum::http::Method::HEAD
        || metadata.method == axum::http::Method::OPTIONS;

    // Pre-shuffle the backend order so attempts hit distinct
    // backends. We don't want an unhealthy first-in-list backend to
    // hot-spot every request.
    let mut order: Vec<usize> = (0..entry.backends.len()).collect();
    {
        let mut rng = rand::rng();
        order.shuffle(&mut rng);
    }
    let has_body = metadata
        .headers
        .get(axum::http::header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value != "0")
        || metadata
            .headers
            .contains_key(axum::http::header::TRANSFER_ENCODING);
    let attempts = if retryable && !has_body {
        MAX_RETRIES.min(order.len())
    } else {
        1
    };

    // Body has to be consumed once. If we want to retry across
    // backends we'd have to buffer it; for v1 we accept "method with
    // body = single attempt" and let clients retry at their layer.
    let body = req.into_body();

    if attempts <= 1 || !retryable {
        let backend = &entry.backends[order[0]];
        return forward_once(state, entry, backend, &metadata, body).await;
    }

    // Buffer the body so we can replay across attempts. For
    // GET/HEAD/OPTIONS the body is normally empty; this is a no-op.
    let bytes = match axum::body::to_bytes(body, 64 * 1024).await {
        Ok(b) => b,
        Err(e) => {
            error!(error = %e, "failed to buffer request body");
            return error_body(StatusCode::BAD_REQUEST, "request body read failed");
        }
    };

    let mut last_err: Option<String> = None;
    for idx in order.iter().take(attempts) {
        let backend = &entry.backends[*idx];
        let body = Body::from(bytes.clone());
        let resp = forward_once(state, entry, backend, &metadata, body).await;
        // Treat 502/504 as retryable transport errors.
        let status = resp.status();
        if status == StatusCode::BAD_GATEWAY || status == StatusCode::GATEWAY_TIMEOUT {
            last_err = Some(format!(
                "backend {} returned {}",
                backend.address,
                status.as_u16()
            ));
            continue;
        }
        return resp;
    }

    warn!(
        host = %entry.host,
        attempts,
        last_err = ?last_err,
        "all backends failed",
    );
    let message = if state.public_mode {
        "application backend unavailable".to_string()
    } else {
        format!(
            "all {} backend(s) for {} failed: {}",
            attempts,
            entry.host,
            last_err.unwrap_or_default()
        )
    };
    error_body(StatusCode::BAD_GATEWAY, &message)
}

struct ForwardRequestMetadata {
    method: axum::http::Method,
    uri: Uri,
    headers: HeaderMap,
    public_peer: Option<PublicPeer>,
}

async fn forward_once(
    state: &ProxyState,
    entry: &RouteEntry,
    backend: &crate::route_store::RouteBackend,
    metadata: &ForwardRequestMetadata,
    body: Body,
) -> Response {
    let path_and_query = metadata
        .uri
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or(metadata.uri.path());
    let upstream_url = format!("http://{}{}", backend.address, path_and_query);

    debug!(
        host = %entry.host,
        upstream = %upstream_url,
        method = %metadata.method,
        "forwarding internal request"
    );

    // Build the outbound request. We translate axum's Method/HeaderMap
    // into reqwest's; reqwest re-uses the http crate types so this is
    // a parse round-trip rather than reflection.
    let mut builder = state.client.request(metadata.method.clone(), &upstream_url);

    for (name, value) in metadata.headers.iter() {
        if HOP_BY_HOP.contains(&name.as_str()) {
            continue;
        }
        builder = builder.header(name.as_str(), value);
    }

    // X-Forwarded-* — give backends visibility into the original
    // request shape. Internal proxy is plain HTTP only, so proto is
    // always "http".
    builder = builder.header("x-forwarded-proto", state.forwarded_proto);
    if let Some(peer) = metadata.public_peer {
        builder = builder
            .header("x-forwarded-for", peer.0.ip().to_string())
            .header("x-real-ip", peer.0.ip().to_string());
    }
    if let Some(orig_host) = metadata.headers.get("host").and_then(|v| v.to_str().ok()) {
        builder = builder
            .header("host", orig_host)
            .header("x-forwarded-host", orig_host);
    }
    if let Some(deployment_id) = entry.deployment_id {
        builder = builder.header("x-temps-deployment-id", deployment_id.to_string());
    }

    // Stream the request body. reqwest accepts a `Body` built from a
    // bytes Stream; we adapt axum's Body via http_body_util.
    let stream = body_to_stream(
        body,
        state.public_mode.then_some(PUBLIC_UPSTREAM_IDLE_TIMEOUT),
    );
    builder = builder.body(reqwest::Body::wrap_stream(stream));

    let upstream = match builder.send().await {
        Ok(r) => r,
        Err(e) => {
            warn!(
                upstream = %upstream_url,
                error = %e,
                "upstream connect failed"
            );
            // reqwest doesn't expose `is_timeout` reliably across
            // versions; rely on the surfaced display string.
            let timeout =
                e.is_timeout() || format!("{e}").to_ascii_lowercase().contains("timed out");
            let status = if timeout {
                StatusCode::GATEWAY_TIMEOUT
            } else {
                StatusCode::BAD_GATEWAY
            };
            let message = if state.public_mode {
                "application backend unavailable".to_string()
            } else {
                format!("upstream {}: {}", backend.address, e)
            };
            return error_body(status, &message);
        }
    };

    // Capture status + headers BEFORE consuming the response body
    // stream — `bytes_stream(self)` takes the response by value.
    let status = upstream.status();
    let upstream_headers_owned = upstream.headers().clone();

    let mut out = Response::new(Body::from_stream(upstream.bytes_stream()));
    *out.status_mut() = status;
    for (name, value) in upstream_headers_owned.iter() {
        if HOP_BY_HOP.contains(&name.as_str()) {
            continue;
        }
        out.headers_mut().insert(name.clone(), value.clone());
    }
    out
}

/// Convert axum's `Body` into a stream of `Result<Bytes, _>` that
/// `reqwest::Body::wrap_stream` accepts. Backpressure preserved.
fn body_to_stream(
    body: Body,
    idle_timeout: Option<Duration>,
) -> impl futures::Stream<Item = Result<bytes::Bytes, std::io::Error>> + Send + 'static {
    use futures::StreamExt;
    use http_body_util::BodyStream;
    futures::stream::unfold(
        (BodyStream::new(body), false),
        move |(mut stream, finished)| async move {
            if finished {
                return None;
            }
            let frame = match idle_timeout {
                Some(timeout) => match tokio::time::timeout(timeout, stream.next()).await {
                    Ok(frame) => frame,
                    Err(_) => {
                        return Some((
                            Err(std::io::Error::new(
                                std::io::ErrorKind::TimedOut,
                                "request body idle timeout",
                            )),
                            (stream, true),
                        ));
                    }
                },
                None => stream.next().await,
            };
            match frame {
                Some(Ok(frame)) => frame
                    .into_data()
                    .ok()
                    .map(|data| (Ok(data), (stream, false))),
                Some(Err(error)) => Some((
                    Err(std::io::Error::new(
                        std::io::ErrorKind::BrokenPipe,
                        error.to_string(),
                    )),
                    (stream, true),
                )),
                None => None,
            }
        },
    )
}

fn extract_host(headers: &HeaderMap) -> Option<String> {
    if headers.get_all("host").iter().count() != 1 {
        return None;
    }
    let authority: axum::http::uri::Authority = headers.get("host")?.to_str().ok()?.parse().ok()?;
    let host = authority.host().trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty() || host.len() > 253 || host.parse::<IpAddr>().is_ok() {
        return None;
    }
    if !host.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    }) {
        return None;
    }
    Some(host)
}

fn error_body(status: StatusCode, message: &str) -> Response {
    let body = format!("{}: {}\n", status.canonical_reason().unwrap_or(""), message);
    let mut resp = (status, body).into_response();
    resp.headers_mut().insert(
        HeaderName::from_static("content-type"),
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    resp
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::route_store::{RouteBackend, RouteEntry, RouteStore};
    use std::path::PathBuf;

    fn route(host: &str, project_id: i32, container_id: &str) -> RouteEntry {
        RouteEntry {
            host: host.to_string(),
            backends: vec![RouteBackend {
                address: format!("{container_id}:3000"),
                container_id: Some(container_id.to_string()),
                container_name: None,
            }],
            deployment_id: Some(project_id * 10),
            project_id: Some(project_id),
            environment_id: Some(project_id * 100),
        }
    }

    #[test]
    fn caller_policy_allows_same_project_container() {
        let store = Arc::new(RouteStore::new(PathBuf::from("/tmp/unused-routes.json")));
        let target = route("prod.alpha.temps.local", 42, "container-alpha");
        store.apply_snapshot(1, vec![target.clone()]);

        assert!(store.container_id_has_project("container-alpha", 42));
    }

    #[test]
    fn caller_policy_blocks_cross_project_container() {
        let store = Arc::new(RouteStore::new(PathBuf::from("/tmp/unused-routes.json")));
        let attacker = route("prod.attacker.temps.local", 7, "container-attacker");
        let victim = route("prod.victim.temps.local", 42, "container-victim");
        store.apply_snapshot(1, vec![attacker, victim.clone()]);

        assert!(!store.container_id_has_project("container-attacker", 42));
    }

    #[tokio::test]
    async fn public_request_body_stream_fails_after_idle_timeout() {
        use futures::StreamExt;

        let pending = futures::stream::pending::<Result<bytes::Bytes, std::io::Error>>();
        let mut stream = Box::pin(body_to_stream(
            Body::from_stream(pending),
            Some(Duration::from_millis(10)),
        ));

        let error = stream
            .next()
            .await
            .expect("timeout emits one terminal error")
            .expect_err("stalled request body must fail");
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        assert!(stream.next().await.is_none());
    }

    #[test]
    fn caller_policy_blocks_unknown_container() {
        let store = Arc::new(RouteStore::new(PathBuf::from("/tmp/unused-routes.json")));
        let target = route("prod.alpha.temps.local", 42, "container-alpha");
        store.apply_snapshot(1, vec![target.clone()]);

        assert!(!store.container_id_has_project("unknown", 42));
    }

    #[test]
    fn caller_identity_lookup_is_memory_only_and_fails_closed() {
        let identities = RwLock::new(CallerIdentitySnapshot::new(HashMap::from([(
            "172.20.1.10".parse().unwrap(),
            "container-alpha".to_string(),
        )])));

        assert_eq!(
            candidate_container_id(&identities, "172.20.1.10".parse().unwrap()).as_deref(),
            Some("container-alpha")
        );
        assert!(candidate_container_id(&identities, "172.20.1.99".parse().unwrap()).is_none());
    }

    #[test]
    fn caller_identity_lookup_rejects_stale_or_cleared_snapshots() {
        let source_ip = "172.20.1.10".parse().unwrap();
        let identities = RwLock::new(CallerIdentitySnapshot {
            identities: HashMap::from([(source_ip, "departed-container".to_string())]),
            refreshed_at: Instant::now() - IDENTITY_MAX_AGE - Duration::from_millis(1),
        });

        assert!(candidate_container_id(&identities, source_ip).is_none());

        let mut snapshot = identities.write();
        snapshot.refreshed_at = Instant::now();
        snapshot.clear();
        drop(snapshot);
        assert!(candidate_container_id(&identities, source_ip).is_none());
    }

    #[test]
    fn identity_events_refresh_only_for_network_ownership_changes() {
        use bollard::models::EventMessageTypeEnum;

        let mut event = bollard::models::EventMessage {
            typ: Some(EventMessageTypeEnum::CONTAINER),
            action: Some("start".to_string()),
            ..Default::default()
        };
        assert!(identity_event_requires_refresh(&event));

        event.action = Some("health_status: healthy".to_string());
        assert!(!identity_event_requires_refresh(&event));

        event.typ = Some(EventMessageTypeEnum::NETWORK);
        event.action = Some("disconnect".to_string());
        assert!(identity_event_requires_refresh(&event));
    }

    #[test]
    fn inspection_quotas_are_bounded_and_project_local() {
        let project_a = InspectionQuota::new();
        let project_b = InspectionQuota::new();

        for _ in 0..IDENTITY_INSPECTION_PROJECT_BURST {
            project_a.tokens.try_acquire().unwrap().forget();
        }
        assert!(project_a.tokens.try_acquire().is_err());
        assert!(project_b.tokens.try_acquire().is_ok());

        let first = project_a.concurrency.try_acquire().unwrap();
        let second = project_a.concurrency.try_acquire().unwrap();
        assert!(project_a.concurrency.try_acquire().is_err());
        drop((first, second));
        assert!(project_a.concurrency.try_acquire().is_ok());
    }

    #[test]
    fn acme_gcra_allows_burst_four_and_refills_twice_per_second() {
        let relay = AcmeRelayState::new();
        for _ in 0..4 {
            assert!(relay.try_take_at(0));
        }
        assert!(!relay.try_take_at(0));
        assert!(relay.try_take_at(ACME_GCRA_PERIOD_MICROS));
        assert!(!relay.try_take_at(ACME_GCRA_PERIOD_MICROS));
        assert!(relay.try_take_at(ACME_GCRA_PERIOD_MICROS * 2));
    }

    #[test]
    fn acme_gcra_concurrent_callers_cannot_exceed_global_burst() {
        let relay = Arc::new(AcmeRelayState::new());
        let start = Arc::new(std::sync::Barrier::new(17));
        let callers = (0..16)
            .map(|_| {
                let relay = Arc::clone(&relay);
                let start = Arc::clone(&start);
                std::thread::spawn(move || {
                    start.wait();
                    relay.try_take_at(0)
                })
            })
            .collect::<Vec<_>>();
        start.wait();
        let admitted = callers
            .into_iter()
            .map(|caller| caller.join().expect("rate-limit caller completes"))
            .filter(|allowed| *allowed)
            .count();
        assert_eq!(admitted, 4);
    }

    #[tokio::test]
    async fn upgraded_tunnel_permit_stays_saturated_until_detached_task_exits() {
        use std::convert::Infallible;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let backend_listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test backend");
        let backend_address = backend_listener.local_addr().expect("backend address");
        let backend = tokio::spawn(async move {
            let (mut socket, _) = backend_listener.accept().await.expect("accept proxy");
            let mut request = Vec::new();
            let mut byte = [0u8; 1];
            tokio::time::timeout(Duration::from_secs(1), async {
                while !request.ends_with(b"\r\n\r\n") {
                    socket
                        .read_exact(&mut byte)
                        .await
                        .expect("read upgrade request");
                    request.push(byte[0]);
                }
            })
            .await
            .expect("backend receives upgrade headers");
            socket
                .write_all(
                    b"HTTP/1.1 101 Switching Protocols\r\nconnection: upgrade\r\nupgrade: websocket\r\n\r\n",
                )
                .await
                .expect("write backend 101");
            let mut drain = [0u8; 32];
            while socket.read(&mut drain).await.expect("drain tunnel") != 0 {}
        });

        let capacity = Arc::new(Semaphore::new(1));
        let proxy_listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test proxy");
        let proxy_address = proxy_listener.local_addr().expect("proxy address");
        let proxy_capacity = Arc::clone(&capacity);
        let proxy = tokio::spawn(async move {
            use hyper_util::rt::{TokioExecutor, TokioIo};
            let (stream, _) = proxy_listener.accept().await.expect("accept client");
            let permit = Arc::new(
                proxy_capacity
                    .try_acquire_owned()
                    .expect("test capacity available"),
            );
            let entry = RouteEntry {
                host: "upgrade.example.test".to_string(),
                backends: vec![RouteBackend {
                    address: backend_address.to_string(),
                    container_id: None,
                    container_name: None,
                }],
                deployment_id: Some(1),
                project_id: Some(1),
                environment_id: Some(1),
            };
            let service = hyper::service::service_fn(move |mut request| {
                request
                    .extensions_mut()
                    .insert(PublicConnectionPermit(Arc::clone(&permit)));
                let entry = entry.clone();
                async move {
                    Ok::<_, Infallible>(proxy_upgrade(&entry, request.map(Body::new), "http").await)
                }
            });
            hyper_util::server::conn::auto::Builder::new(TokioExecutor::new())
                .serve_connection_with_upgrades(TokioIo::new(stream), service)
                .await
                .expect("serve upgraded connection");
        });

        let mut client = tokio::net::TcpStream::connect(proxy_address)
            .await
            .expect("connect test client");
        client
            .write_all(
                b"GET /socket HTTP/1.1\r\nhost: upgrade.example.test\r\nconnection: upgrade\r\nupgrade: websocket\r\n\r\n",
            )
            .await
            .expect("write client upgrade");
        let mut response = Vec::new();
        let mut byte = [0u8; 1];
        tokio::time::timeout(Duration::from_secs(1), async {
            while !response.ends_with(b"\r\n\r\n") {
                client.read_exact(&mut byte).await.expect("read proxy 101");
                response.push(byte[0]);
            }
        })
        .await
        .expect("client receives proxy 101");
        assert!(response.starts_with(b"HTTP/1.1 101"));
        tokio::time::timeout(Duration::from_secs(1), proxy)
            .await
            .expect("HTTP connection task exits after handing off upgrade")
            .expect("proxy task exits");
        assert!(capacity.clone().try_acquire_owned().is_err());

        drop(client);
        tokio::time::timeout(Duration::from_secs(1), backend)
            .await
            .expect("backend exits after upgraded client closes")
            .expect("backend task exits");
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if capacity.clone().try_acquire_owned().is_ok() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("capacity releases after upgraded socket closes");
    }

    #[tokio::test]
    async fn tunnel_idle_deadline_tracks_activity_across_both_directions() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let (mut client, tunnel_client) = tokio::io::duplex(256);
        let (tunnel_backend, mut backend) = tokio::io::duplex(256);
        let tunnel = tokio::spawn(copy_tunnel_with_idle(
            tunnel_client,
            tunnel_backend,
            Duration::from_millis(80),
        ));

        for byte in 0u8..6 {
            client.write_all(&[byte]).await.expect("write test byte");
            let mut received = [0u8; 1];
            backend
                .read_exact(&mut received)
                .await
                .expect("read forwarded test byte");
            assert_eq!(received[0], byte);
            tokio::time::sleep(Duration::from_millis(30)).await;
            assert!(
                !tunnel.is_finished(),
                "one-way activity must reset shared idle deadline"
            );
        }

        let result = tokio::time::timeout(Duration::from_millis(200), tunnel)
            .await
            .expect("fully idle tunnel must terminate")
            .expect("tunnel task joins");
        assert_eq!(
            result.expect_err("idle tunnel returns timeout").kind(),
            std::io::ErrorKind::TimedOut
        );
    }
}
