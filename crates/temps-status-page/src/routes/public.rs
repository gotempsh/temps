// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Unauthenticated, read-only view of published status pages (#1412).
//!
//! Mounted on the public router (ADR-051), so it is reachable on the public
//! ingress surface even when the console is restricted. The handlers:
//!
//! - authenticate the *request* by its unguessable slug, never a session;
//! - read only the in-memory [`StatusSnapshotCache`]: no database access, no
//!   probes, no work proportional to anything a caller controls;
//! - answer every slug that is not a published page — unknown, malformed,
//!   unpublished, deleted, rotated — with byte-identical 404s;
//! - are rate-limited per client IP with a global ceiling.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::{
    extract::{rejection::PathRejection, ConnectInfo, Path, Request, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};

use crate::services::public_pages::{
    is_well_formed_slug, render::NOT_FOUND_HTML, PublicStatusPageResponse, StatusSnapshotCache,
};

/// Requests per client IP per window.
pub const PER_CLIENT_LIMIT: u32 = 300;
/// Requests per window, shared, from clients that could not be tracked
/// individually because the client table was full. Tracked clients within
/// their own limit never count against it, so a flood of distinct addresses
/// can exhaust this budget but cannot take the page away from visitors who
/// are already tracked.
pub const OVERFLOW_LIMIT: u32 = 60_000;
/// Length of a rate-limit window.
pub const RATE_WINDOW: Duration = Duration::from_secs(60);
const SHARDS: usize = 16;
/// Tracked clients per shard; 16 × 4096 bounds memory to ~65k entries.
const MAX_CLIENTS_PER_SHARD: usize = 4096;

use temps_core::public_status_page::{NOT_FOUND_JSON, SECURITY_HEADERS, STATUS_HOST_HEADER};

/// Fixed-window limiter keyed by client (IPv4 address or IPv6 /64), sharded
/// so concurrent requests rarely contend.
///
/// Saturation policy: when a shard is full of clients active in the current
/// window, new clients are not tracked; they share the atomic
/// [`OVERFLOW_LIMIT`] budget instead. A flood of distinct addresses therefore
/// cannot evict real visitors' counters, cannot make tracked visitors receive
/// 429s, and memory stays bounded. Rejections are counted and logged at most
/// once per window, so an attack cannot flood the log.
pub struct PublicRateLimiter {
    epoch: Instant,
    per_client_limit: u32,
    overflow_limit: u32,
    max_clients_per_shard: usize,
    window: Duration,
    /// High 32 bits: window index. Low 32 bits: overflow requests in it.
    overflow: AtomicU64,
    /// Requests rejected since the process started.
    rejected: AtomicU64,
    /// Window index (+1) in which a rejection was last logged; 0 = never.
    last_logged_window: AtomicU64,
    shards: Vec<Mutex<Shard>>,
}

#[derive(Default)]
struct Shard {
    /// Client -> (window index, requests in that window).
    clients: HashMap<String, (u64, u32)>,
    /// Window in which stale clients were last swept; a full shard is swept
    /// at most once per window so a flood of new addresses stays O(1).
    swept_window: Option<u64>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct RateLimited {
    pub retry_after_secs: u64,
    /// `Some(total rejections)` for the first rejection of a window, so the
    /// caller logs once per window instead of once per request.
    pub log_total: Option<u64>,
}

/// The rate-limit key for a client address: the address itself for IPv4, the
/// /64 prefix for IPv6 (one host or subscriber typically owns a whole /64),
/// and the raw string for anything that does not parse.
pub fn client_key(client: &str) -> String {
    match client.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V6(v6)) => match v6.to_ipv4_mapped() {
            Some(v4) => v4.to_string(),
            None => {
                let prefix = u128::from(v6) & !((1u128 << 64) - 1);
                format!("{}/64", std::net::Ipv6Addr::from(prefix))
            }
        },
        Ok(ip) => ip.to_string(),
        Err(_) => client.to_string(),
    }
}

impl Default for PublicRateLimiter {
    fn default() -> Self {
        Self::new(PER_CLIENT_LIMIT, OVERFLOW_LIMIT, RATE_WINDOW)
    }
}

impl PublicRateLimiter {
    pub fn new(per_client_limit: u32, overflow_limit: u32, window: Duration) -> Self {
        Self::with_capacity(
            per_client_limit,
            overflow_limit,
            window,
            MAX_CLIENTS_PER_SHARD,
        )
    }

    pub fn with_capacity(
        per_client_limit: u32,
        overflow_limit: u32,
        window: Duration,
        max_clients_per_shard: usize,
    ) -> Self {
        Self {
            epoch: Instant::now(),
            per_client_limit,
            overflow_limit,
            max_clients_per_shard,
            window,
            overflow: AtomicU64::new(0),
            rejected: AtomicU64::new(0),
            last_logged_window: AtomicU64::new(0),
            shards: (0..SHARDS).map(|_| Mutex::new(Shard::default())).collect(),
        }
    }

    fn reject(&self, window: u64, retry_after_secs: u64) -> RateLimited {
        let total = self.rejected.fetch_add(1, Ordering::Relaxed) + 1;
        let first_this_window =
            self.last_logged_window.swap(window + 1, Ordering::AcqRel) != window + 1;
        RateLimited {
            retry_after_secs,
            log_total: first_this_window.then_some(total),
        }
    }

    pub fn check(&self, client: &str) -> Result<(), RateLimited> {
        self.check_at(client, Instant::now())
    }

    pub fn check_at(&self, client: &str, now: Instant) -> Result<(), RateLimited> {
        let window_secs = self.window.as_secs().max(1);
        let elapsed = now.saturating_duration_since(self.epoch).as_secs();
        let window = elapsed / window_secs;
        let retry_after_secs = window_secs - (elapsed % window_secs);
        let client = client_key(client);
        let client = client.as_str();

        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        client.hash(&mut hasher);
        let shard = &self.shards[(hasher.finish() as usize) % self.shards.len()];
        {
            // A poisoned shard only means another request panicked mid-update
            // of a counter; the map is still structurally valid.
            let mut shard = shard.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(entry) = shard.clients.get_mut(client) {
                if entry.0 != window {
                    *entry = (window, 0);
                }
                if entry.1 >= self.per_client_limit {
                    return Err(self.reject(window, retry_after_secs));
                }
                entry.1 += 1;
                return Ok(());
            }
            if shard.clients.len() >= self.max_clients_per_shard
                && shard.swept_window != Some(window)
            {
                shard.clients.retain(|_, (w, _)| *w == window);
                shard.swept_window = Some(window);
            }
            if shard.clients.len() < self.max_clients_per_shard {
                shard.clients.insert(client.to_string(), (window, 1));
                return Ok(());
            }
        }

        // Untracked client: admit against the shared overflow budget.
        let admitted = self
            .overflow
            .try_update(Ordering::AcqRel, Ordering::Acquire, |packed| {
                let (w, count) = (packed >> 32, (packed & 0xFFFF_FFFF) as u32);
                if w != window {
                    Some((window << 32) | 1)
                } else if count >= self.overflow_limit {
                    None
                } else {
                    Some((window << 32) | u64::from(count + 1))
                }
            });
        if admitted.is_err() {
            return Err(self.reject(window, retry_after_secs));
        }
        Ok(())
    }
}

/// State of the public router. Holds its own handles; nothing else.
pub struct PublicStatusState {
    pub cache: Arc<StatusSnapshotCache>,
    pub limiter: PublicRateLimiter,
}

impl PublicStatusState {
    pub fn new(cache: Arc<StatusSnapshotCache>) -> Self {
        Self {
            cache,
            limiter: PublicRateLimiter::default(),
        }
    }
}

/// The custom hostname the proxy served this request for (#1412), if any.
/// The proxy strips any client-supplied copy of the header and sets it only
/// on its custom-hostname path. A value that is not valid UTF-8 matches no
/// hostname, so it can only ever narrow what is served.
fn custom_host(headers: &HeaderMap) -> Option<String> {
    headers.get(STATUS_HOST_HEADER).map(|value| {
        value
            .to_str()
            .map(|host| host.trim().trim_end_matches('.').to_ascii_lowercase())
            .unwrap_or_default()
    })
}

fn harden(response: &mut Response) {
    let headers = response.headers_mut();
    for (name, value) in SECURITY_HEADERS {
        headers.insert(name, HeaderValue::from_static(value));
    }
}

async fn rate_limit(
    State(state): State<Arc<PublicStatusState>>,
    request: Request,
    next: Next,
) -> Response {
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|info| info.0);
    let client = temps_core::resolve_client_ip(request.headers(), peer);
    match state.limiter.check(&client) {
        Ok(()) => next.run(request).await,
        Err(limited) => {
            if let Some(total) = limited.log_total {
                tracing::warn!(
                    rejected_total = total,
                    retry_after_secs = limited.retry_after_secs,
                    "public status page requests are being rate-limited (logged once per window)"
                );
            }
            let mut response = (
                StatusCode::TOO_MANY_REQUESTS,
                [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                "Too many requests",
            )
                .into_response();
            harden(&mut response);
            if let Ok(value) = HeaderValue::from_str(&limited.retry_after_secs.to_string()) {
                response.headers_mut().insert(header::RETRY_AFTER, value);
            }
            response
        }
    }
}

/// Public status page (HTML)
#[utoipa::path(
    get,
    path = "/status/{slug}",
    params(("slug" = String, Path, description = "Unguessable status page token")),
    responses(
        (status = 200, description = "Rendered status page", content_type = "text/html"),
        (status = 404, description = "No published status page at this URL"),
        (status = 429, description = "Rate limited"),
    ),
    tag = "Status Page"
)]
pub async fn get_public_status_page(
    State(state): State<Arc<PublicStatusState>>,
    headers: HeaderMap,
    slug: Result<Path<String>, PathRejection>,
) -> Response {
    // A slug that does not even decode gets the same 404 as any other miss.
    let slug = slug.map(|Path(slug)| slug).unwrap_or_default();
    let snapshot = is_well_formed_slug(&slug)
        .then(|| state.cache.get(&slug))
        .flatten()
        .filter(|snapshot| snapshot.serves_host(custom_host(&headers).as_deref()));
    let mut response = match snapshot {
        Some(snapshot) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            snapshot.html.clone(),
        )
            .into_response(),
        None => (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            NOT_FOUND_HTML,
        )
            .into_response(),
    };
    harden(&mut response);
    response
}

/// Public status page (JSON)
#[utoipa::path(
    get,
    path = "/status/{slug}/summary.json",
    params(("slug" = String, Path, description = "Unguessable status page token")),
    responses(
        (status = 200, description = "Status page snapshot", body = PublicStatusPageResponse),
        (status = 404, description = "No published status page at this URL"),
        (status = 429, description = "Rate limited"),
    ),
    tag = "Status Page"
)]
pub async fn get_public_status_page_json(
    State(state): State<Arc<PublicStatusState>>,
    headers: HeaderMap,
    slug: Result<Path<String>, PathRejection>,
) -> Response {
    let slug = slug.map(|Path(slug)| slug).unwrap_or_default();
    let snapshot = is_well_formed_slug(&slug)
        .then(|| state.cache.get(&slug))
        .flatten()
        .filter(|snapshot| snapshot.serves_host(custom_host(&headers).as_deref()));
    let mut response = match snapshot {
        Some(snapshot) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            snapshot.json.clone(),
        )
            .into_response(),
        None => (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "application/problem+json")],
            NOT_FOUND_JSON,
        )
            .into_response(),
    };
    harden(&mut response);
    // Public, credential-free data: any origin may read it (status widgets).
    response.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    response
}

/// Router for the public status page endpoints, rate limit included.
pub fn create_public_router(state: Arc<PublicStatusState>) -> Router {
    Router::new()
        .route("/status/{slug}", get(get_public_status_page))
        .route(
            "/status/{slug}/summary.json",
            get(get_public_status_page_json),
        )
        .layer(middleware::from_fn_with_state(state.clone(), rate_limit))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn per_client_limit_resets_each_window() {
        let limiter = PublicRateLimiter::new(3, 1000, Duration::from_secs(60));
        let t0 = limiter.epoch;
        for _ in 0..3 {
            assert!(limiter.check_at("198.51.100.1", t0).is_ok());
        }
        let limited = limiter
            .check_at("198.51.100.1", t0)
            .expect_err("fourth request in the window is limited");
        assert_eq!(limited.retry_after_secs, 60);
        assert_eq!(limited.log_total, Some(1));
        assert!(
            limiter.check_at("198.51.100.2", t0).is_ok(),
            "other clients are unaffected"
        );
        assert!(limiter
            .check_at("198.51.100.1", t0 + Duration::from_secs(60))
            .is_ok());
    }

    #[test]
    fn untracked_clients_share_the_overflow_budget() {
        // Capacity 0: no client can be tracked, so all share the budget.
        let limiter = PublicRateLimiter::with_capacity(100, 5, Duration::from_secs(60), 0);
        let t0 = limiter.epoch;
        for i in 0..5 {
            assert!(limiter.check_at(&format!("192.0.2.{i}"), t0).is_ok());
        }
        assert!(limiter.check_at("192.0.2.200", t0).is_err());
        assert!(limiter
            .check_at("192.0.2.200", t0 + Duration::from_secs(61))
            .is_ok());
    }

    #[test]
    fn an_overflow_flood_does_not_limit_tracked_clients() {
        let limiter = PublicRateLimiter::with_capacity(10, 1, Duration::from_secs(60), 1);
        let t0 = limiter.epoch;
        // Track one visitor per shard (at most), then exhaust the overflow.
        assert!(limiter.check_at("198.51.100.7", t0).is_ok());
        for i in 0..10_000 {
            let _ = limiter.check_at(&format!("10.0.{}.{}", i >> 8, i & 255), t0);
        }
        for _ in 0..9 {
            assert!(
                limiter.check_at("198.51.100.7", t0).is_ok(),
                "a tracked visitor keeps its own budget during a flood"
            );
        }
    }

    #[test]
    fn ipv6_clients_are_keyed_by_their_64_prefix() {
        assert_eq!(
            client_key("2001:db8:1:2:aaaa:bbbb:cccc:dddd"),
            client_key("2001:db8:1:2::1")
        );
        assert_ne!(client_key("2001:db8:1:2::1"), client_key("2001:db8:1:3::1"));
        assert_eq!(client_key("::ffff:192.0.2.1"), "192.0.2.1");
        assert_eq!(client_key("192.0.2.1"), "192.0.2.1");
        let limiter = PublicRateLimiter::new(2, 1000, Duration::from_secs(60));
        let t0 = limiter.epoch;
        assert!(limiter.check_at("2001:db8:1:2::1", t0).is_ok());
        assert!(limiter.check_at("2001:db8:1:2::2", t0).is_ok());
        assert!(
            limiter.check_at("2001:db8:1:2::3", t0).is_err(),
            "rotating addresses within a /64 does not reset the budget"
        );
    }

    #[test]
    fn rejections_are_logged_once_per_window() {
        let limiter = PublicRateLimiter::new(1, 1000, Duration::from_secs(60));
        let t0 = limiter.epoch;
        assert!(limiter.check_at("192.0.2.1", t0).is_ok());
        let first = limiter.check_at("192.0.2.1", t0).expect_err("limited");
        let second = limiter.check_at("192.0.2.1", t0).expect_err("limited");
        assert_eq!(first.log_total, Some(1));
        assert_eq!(second.log_total, None);
        assert!(limiter
            .check_at("192.0.2.1", t0 + Duration::from_secs(60))
            .is_ok());
        let next = limiter
            .check_at("192.0.2.1", t0 + Duration::from_secs(60))
            .expect_err("limited");
        assert_eq!(next.log_total, Some(3));
    }

    #[test]
    fn client_tracking_memory_is_bounded() {
        let limiter = PublicRateLimiter::new(1, u32::MAX, Duration::from_secs(60));
        let t0 = limiter.epoch;
        for i in 0..(SHARDS * MAX_CLIENTS_PER_SHARD + 5000) {
            let _ = limiter.check_at(
                &format!("10.{}.{}.{}", i >> 16, (i >> 8) & 255, i & 255),
                t0,
            );
        }
        let tracked: usize = limiter
            .shards
            .iter()
            .map(|s| s.lock().map(|shard| shard.clients.len()).unwrap_or(0))
            .sum();
        assert!(tracked <= SHARDS * MAX_CLIENTS_PER_SHARD);
    }
}
