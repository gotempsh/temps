// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Proxy request pipeline for the Temps reverse proxy.
//!
//! # Hot-path invariant
//!
//! No function on the per-request path (`early_request_filter`, `request_filter`,
//! `upstream_peer`, `upstream_response_filter`, `response_filter`, and every helper
//! they call) may await a database query directly.
//!
//! - **Writes** go through the `ProxyLogBatchHandle` / `TrackingBatchHandle` mpsc
//!   channels and are flushed by the background batch writer.
//! - **Reads** go through ArcSwap snapshots (refreshed by background loops) or
//!   moka TTL caches (populated on first miss, then served in-memory).
//!
//! The ACME HTTP-01 challenge lookup (`handle_acme_http_challenge`) is no
//! exception: it reads [`AcmeChallengeCache`], whose misses share one
//! single-flight, rate-limited reload, so a flood of random tokens on the
//! unauthenticated `/.well-known/acme-challenge/*` path cannot fan out into
//! one query per request.

use crate::handler::preview_wall::{
    build_logout_cookie_sandbox, build_logout_cookie_sandbox_unpartitioned,
    generate_preview_bridge_html, generate_preview_form_html_labeled, sanitize_next,
    PREVIEW_LOGIN_PATH, PREVIEW_LOGOUT_PATH,
};
use crate::on_demand::OnDemandManager;
use crate::preview_auth::{
    build_set_cookie_sandbox, check_preview_auth, combine_cookie_header_values,
    encode_preview_cookie_subject, extract_cookie_values, parse_preview_host,
    preview_cookie_needs_refresh, preview_gateway_peer, preview_request_group_key, verify_argon2,
    PreviewAuthLimiter, PreviewAuthOutcome, PreviewHost, PreviewRequest, SandboxLookupCache,
};
use crate::service::acme_challenge_cache::AcmeChallengeCache;
use crate::service::cert_host_cache::CertHostCache;
use crate::service::cookie_codec::{
    make_v2_session_payload, parse_session_cookie, parse_visitor_cookie,
};
use crate::service::ip_access_control_service::IpAccessControlService;
use crate::service::proxy_log_batch_writer::{
    ProxyLogBatchHandle, TrackingBatchHandle, TrackingEvent,
};
use crate::service::proxy_log_service::CreateProxyLogRequest;
use crate::static_file_serving::{
    bounded_cas_etag, bounded_log_value, canonical_redirect_location, cap_static_chunk,
    if_none_match_matches, metadata_etag, object_etag, open_static_file, opened_cas_size_matches,
    read_static_chunk, resolve_static_object_request, static_mount_prefix,
    static_not_found_contract, static_object_key, unavailable_outcome, StaticFileMatch,
    StaticFileServeOutcome, STATIC_NOT_FOUND_BODY,
};
use crate::tls_fingerprint;
use crate::traits::*;
use async_trait::async_trait;
use axum::http::{header, uri::Authority};
use bytes::Bytes;
use cookie::Cookie;
use pingora::http::StatusCode;
use pingora::Error;
use pingora_core::protocols::http::compression::ResponseCompressionCtx;
use pingora_core::{
    upstreams::peer::{HttpPeer, Peer},
    Result,
};
use pingora_http::{RequestHeader, ResponseHeader};
use pingora_proxy::{FailToProxy, ProxyHttp, Session as PingoraSession};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;
use temps_config::deployment_config::EnvironmentDeploymentConfigExt;
use temps_core::static_files::{normalize_static_request_path, MAX_PUBLIC_STATIC_ASSET_BYTES};
use temps_database::DbConnection;
use temps_entities::{deployments, environments, projects};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

mod acme;
mod edge_rules;
mod markdown;
mod policy;
mod security_headers;
mod static_serving;
mod timeouts;
mod visitor;

#[cfg(test)]
mod tests;

pub use acme::ACME_HTTP01_PREFIX;
use markdown::{
    apply_markdown_response_headers, apply_markdown_upstream_gate, downstream_response_version,
    request_identity_encoding_for_markdown, response_body_filter_inner, restore_client_compression,
    RESPONSE_COMPRESSION_LEVEL,
};
use policy::{evaluate_request_policy, AccessRequest, IP_DENY_LOG_LIMITER};
pub(crate) use policy::{legacy_ip_gate_denies, normalize_client_ip};
use security_headers::strip_proxy_owned_response_headers;
use timeouts::{
    control_plane_address, resolve_customer_io_timeout, upstream_io_timeout, TimeoutTrafficKind,
};
use visitor::{get_session_cookie_name, get_visitor_cookie_name};
pub use visitor::{SESSION_ID_COOKIE, VISITOR_ID_COOKIE};

// Constants

pub const ROUTE_PREFIX_TEMPS: &str = "/api/_temps";

pub const SERVER_NAME: &[u8; 5] = b"Temps";
pub const LB_SEED: u64 = 42;
pub const MAX_WEBHOOK_BODY_SIZE: usize = 16 * 1024;
pub const LOG_STATIC_ASSETS: bool = false;

/// Path prefix for control-plane↔node cluster management (registration,
/// heartbeat, DNS sync). A worker joining the cluster speaks plaintext HTTP
/// before any TLS material exists, so these calls must never be answered with
/// a redirect to a certificate that has not been issued yet.
pub const INTERNAL_CLUSTER_PREFIX: &str = "/api/internal/";

fn should_lookup_sleeping_environment(
    route_table: Option<&temps_routes::CachedPeerTable>,
    host: &str,
) -> bool {
    !route_table.is_some_and(|routes| routes.owns_hostname(host))
}

/// Decide whether a request on the plain-HTTP listener should be answered with
/// a 301 to the HTTPS URL.
///
/// The inputs, in the order they are consulted:
///
/// - `globally_disabled` — the `disable_https_redirect` operator kill switch
///   (set by the service unit in local/testing mode). Master off; nothing
///   overrides it, including a per-environment `force_https = true`, so a local
///   rig never starts bouncing developers to a port with no certificate.
/// - `is_tls` — already HTTPS, nothing to do.
/// - `path` — anything under [`ACME_HTTP01_PREFIX`] is exempt unconditionally.
///   A 301 here breaks issuance and, worse, silent renewal: the CA follows the
///   redirect to an HTTPS endpoint whose certificate is precisely the one that
///   has expired or does not exist yet. This exemption applies even when the
///   host has a valid certificate, because renewal happens while the old
///   certificate is still installed. [`INTERNAL_CLUSTER_PREFIX`] is exempt for
///   the same reason: a worker registering with the control plane speaks
///   plaintext HTTP before it holds any TLS material, and those calls land on
///   the console host — the one an operator is most likely to force to HTTPS.
/// - `env_force_https` — the per-environment override. `None` inherits
///   `host_has_cert`; `Some(b)` wins outright.
/// - `host_has_cert` — the default heuristic: redirect only hosts that actually
///   completed TLS provisioning, so HTTP-only installs are never redirected.
///
/// Kept as a free function over plain values so the decision table is unit
/// testable without a live session, and so the hot path stays allocation-free.
/// `host_has_cert` is a closure rather than a `bool` to preserve the
/// short-circuit the original `&&` chain had: the overwhelmingly common case is
/// an HTTPS request, which must not pay for a cert-cache snapshot read, and an
/// environment with an explicit override never needs the lookup at all.
fn should_redirect_to_https(
    globally_disabled: bool,
    is_tls: bool,
    path: &str,
    env_force_https: Option<bool>,
    host_has_cert: impl FnOnce() -> bool,
) -> bool {
    if globally_disabled || is_tls {
        return false;
    }

    if path.starts_with(ACME_HTTP01_PREFIX) || path.starts_with(INTERNAL_CLUSTER_PREFIX) {
        return false;
    }

    env_force_https.unwrap_or_else(host_has_cert)
}

fn https_redirect_response(redirect_url: &str, request_id: &str) -> Result<ResponseHeader> {
    let mut response = ResponseHeader::build(301, None)?;
    response.insert_header("Location", redirect_url)?;
    response.insert_header("Content-Length", "0")?;
    response.insert_header("X-Request-ID", request_id)?;
    response.insert_header("X-Temps-Proxy-Https-Redirect", "1")?;
    response.insert_header("X-Temps-Proxy-Probe-Capable", "1")?;
    Ok(response)
}

/// Build the `Location` for a custom domain's "Redirect to" setting.
///
/// The stored target is an absolute URL (the domain service normalizes a bare
/// host to `https://host`). When it names only an origin — no path beyond `/`,
/// no query, no fragment — the request's path and query are carried over, so
/// `www.example.com/pricing?x=1` lands on `example.com/pricing?x=1` instead of
/// the target's homepage. A target that spells out its own path or query is a
/// deliberate "send everything here" redirect and is returned verbatim, which
/// is what every redirect did before path preservation existed.
///
/// `path` is the raw request path from the URI (still percent-encoded), so no
/// re-encoding happens here.
fn domain_redirect_location(target: &str, path: &str, query: Option<&str>) -> String {
    let Some(scheme_end) = target.find("://") else {
        // Not an absolute URL (legacy row written before normalization):
        // keep the old behaviour rather than guess how to join it.
        return target.to_string();
    };
    let after_scheme = &target[scheme_end + 3..];
    let rest = after_scheme
        .find(['/', '?', '#'])
        .map_or("", |idx| &after_scheme[idx..]);
    if !rest.is_empty() && rest != "/" {
        return target.to_string();
    }

    let origin = &target[..scheme_end + 3 + after_scheme.len() - rest.len()];
    let path = if path.is_empty() { "/" } else { path };
    match query.filter(|q| !q.is_empty()) {
        Some(query) => format!("{origin}{path}?{query}"),
        None => format!("{origin}{path}"),
    }
}

fn deployment_asset_scope(
    current_deployment_slug: &str,
    current_environment_id: i32,
    current_deployment_id: i32,
    source_deployment_slug: Option<&str>,
    source_environment_id: Option<i32>,
    source_deployment_id: Option<i32>,
    requested_deployment_slug: &str,
) -> Option<(i32, i32)> {
    if current_deployment_slug == requested_deployment_slug {
        return Some((current_environment_id, current_deployment_id));
    }

    (source_deployment_slug == Some(requested_deployment_slug))
        .then(|| Some((source_environment_id?, source_deployment_id?)))
        .flatten()
}

fn legacy_deployment_asset_scope(
    current_deployment_slug: &str,
    origin: &crate::service::static_asset_lookup::LegacyAssetOrigin,
    requested_deployment_slug: &str,
) -> Option<(i32, i32)> {
    (requested_deployment_slug == current_deployment_slug
        || requested_deployment_slug == origin.slug)
        .then_some((origin.environment_id, origin.deployment_id))
}

fn inherited_https_policy(production_https: bool, host_has_cert: bool) -> bool {
    production_https || host_has_cert
}

/// Whether the "production" HTTPS-by-default assumption (no `external_url`
/// configured -> treat every host as production) should even be consulted for
/// this request.
///
/// Scoped to resolved project traffic only (`has_environment`): requests whose
/// `Host` never matched a project domain — the admin/console UI, and internal
/// cluster-management API calls such as node registration/heartbeat — must
/// never be redirected off this default. Cluster bootstrap traffic runs over
/// plaintext HTTP by design, before any TLS material exists to redirect to.
///
/// The console deliberately does **not** ride on this default. An `https://`
/// `external_url` says how users reach the platform, not who terminates the
/// TLS: with an upstream CDN or reverse proxy in front, Temps sees a plaintext
/// connection and holds no certificate, so redirecting would bounce the
/// browser back to the CDN and into an infinite loop. Operators who want the
/// console forced to HTTPS say so explicitly via
/// `AppSettings::console_force_https`, which flows in through the same
/// `force_https` parameter an environment uses.
fn should_apply_production_https_default(
    disable_https_redirect: bool,
    is_tls: bool,
    path: &str,
    force_https: Option<bool>,
    has_environment: bool,
) -> bool {
    !disable_https_redirect
        && !is_tls
        && !path.starts_with(ACME_HTTP01_PREFIX)
        && !path.starts_with(INTERNAL_CLUSTER_PREFIX)
        && force_https.is_none()
        && has_environment
}

/// Proxy context for tracking request state
pub struct ProxyContext {
    pub response_modified: bool,
    pub response_compressed: bool,
    pub upstream_response_headers: Option<ResponseHeader>,
    pub content_type: Option<String>,
    pub buffer: Vec<u8>,
    pub project: Option<Arc<projects::Model>>,
    pub environment: Option<Arc<environments::Model>>,
    pub deployment: Option<Arc<deployments::Model>>,
    pub request_id: String,
    pub start_time: Instant,
    pub method: String,
    pub path: String,
    pub query_string: Option<String>,
    pub host: String,
    pub user_agent: String,
    pub referrer: Option<String>,
    pub ip_address: Option<String>,
    pub visitor_id: Option<String>,
    pub session_id: Option<String>,
    pub is_new_session: bool,
    pub request_headers: Option<HashMap<String, String>>,
    pub response_headers: Option<HashMap<String, String>>,
    pub request_visitor_cookie: Option<String>,
    pub request_session_cookie: Option<String>,
    pub is_sse: bool,
    pub is_websocket: bool,
    pub skip_tracking: bool,
    pub routing_status: String,
    pub error_message: Option<String>,
    pub upstream_host: Option<String>,
    pub container_id: Option<String>,
    pub container_name: Option<String>,
    pub tls_fingerprint: Option<String>,
    pub tls_version: Option<String>,
    pub tls_cipher: Option<String>,
    /// SNI hostname from TLS handshake (for SNI-based routing)
    pub sni_hostname: Option<String>,
    /// Upstream response body bytes actually forwarded to the client,
    /// accumulated per-chunk in `response_body_filter`. Authoritative source
    /// for response bandwidth — unlike the `Content-Length` header, this is
    /// always populated even for chunked/streamed responses.
    pub upstream_body_bytes_received: usize,
    /// Client request body bytes received, accumulated per-chunk in
    /// `request_body_filter`. Authoritative source for request bandwidth —
    /// unlike the `Content-Length` header, this is always populated even for
    /// chunked-encoded request bodies.
    pub client_body_bytes_received: usize,
    /// Proxy log entry built in `log_request` (response-header time), held
    /// here rather than sent immediately because `upstream_body_bytes_received`
    /// isn't fully accumulated until the response body finishes streaming.
    /// The `logging` hook patches in the final byte count and sends it.
    pub pending_proxy_log: Option<CreateProxyLogRequest>,
    /// Whether the client requested a Markdown response via `Accept: text/markdown`
    pub wants_markdown: bool,
    /// Accumulated body bytes for HTML-to-Markdown conversion
    pub markdown_buffer: Vec<u8>,
    /// The client's `Accept-Encoding`, saved before a Markdown request rewrites
    /// it to `identity`, so compression can be restored if the response is
    /// passed through unconverted. `None` when compression was off anyway
    /// (streaming requests) or the client sent none.
    pub markdown_fallback_accept_encoding: Option<String>,
    /// Number of upstream connection attempts (for retry logic)
    pub upstream_connect_tries: usize,
    /// Time upstream took to accept the request body (upload diagnostics, Pingora 0.8.0)
    pub upstream_write_pending_time_ms: Option<i32>,
    /// When `upstream_peer` started resolving/connecting the upstream. Basis
    /// for the backend-latency metric; `None` for requests the proxy answered
    /// itself (static files, redirects, walls).
    pub upstream_start_time: Option<Instant>,
    /// Backend latency: `upstream_start_time` → first upstream response
    /// header (connect + request + upstream processing + TTFB).
    pub upstream_response_time_ms: Option<u64>,
    /// Set when the request matched a workspace preview hostname and passed
    /// auth — `upstream_peer` will route it to the local preview gateway.
    pub preview_route: Option<PreviewHost>,
    /// The previewed sandbox runs on a worker node: `upstream_peer` sends the
    /// request to this process's worker preview relay instead of the gateway.
    pub preview_on_worker: bool,
    /// The admin gate denied this request, so `upstream_peer` must send it to
    /// the listener that serves only the console's public routes (ADR-051)
    /// instead of resolving a peer. A non-allowlisted client can therefore
    /// reach a tracking pixel or a provider webhook, and structurally nothing
    /// else, because nothing else is mounted there.
    pub public_surface_only: bool,
    /// The request is for a status page's custom hostname (#1412) and was
    /// rewritten to that page's route; `upstream_peer` sends it to the
    /// public-only listener and nowhere else.
    pub status_page_host: bool,
    /// The upstream confirmed a long-lived stream (SSE `text/event-stream`, or
    /// a `101` WebSocket upgrade). Such a session's total duration is a
    /// connection lifetime, not a latency, so `logging` keeps it out of the
    /// duration histograms — see [`crate::metrics::ProxyMetrics::record`].
    pub streaming_session: bool,
    /// Reserved in-flight slot for this request's project/environment, held
    /// for the whole request lifetime and released when dropped in
    /// `logging()`. `None` when no cap applies (unlimited, or no
    /// project/environment resolved for this request — e.g. console/preview
    /// traffic).
    pub connection_permit: Option<crate::connection_limiter::ConnectionPermit>,
    /// Set when the host's environment hands this path to another service of
    /// its project (ADR 050). Resolved once from the in-memory route table;
    /// project context, static serving and the upstream peer all follow the
    /// target instead of the host's own service.
    pub path_route: Option<temps_routes::PathRoute>,
    /// The host is a signed-in address (ADR 065 D8): the app's response may
    /// not set the gate's cookies or pass for the gate.
    pub gated_host: bool,
    /// Who the gate admitted, for a signed-in address.
    pub gate_identity: Option<temps_core::gate::GateClaims>,
    /// What Shield's lists and signals say about the request (ADR 066 D1),
    /// when it is not `pass`. Recorded with the request log in every mode.
    pub shield: Option<temps_shield::Verdict>,
    /// The `X-Temps-Shield` verdict, set only when Shield acted (D11).
    pub shield_header: Option<&'static str>,
    /// The environment Shield counted this request for, so its response
    /// status feeds the same environment's adaptive difficulty (D6).
    pub shield_env: Option<i32>,
    /// The line netwatch gets for this request once its status is known
    /// (ADR 066 D12); `None` without netwatch.
    pub netwatch_line: Option<temps_shield::netwatch::RequestLine>,
    /// The client's JA4 and where it came from (ADR 066 D4), resolved with
    /// the client address. Recorded with the request log; nothing acts on
    /// it yet.
    pub client_fingerprint: Option<temps_shield::ja4::ClientFingerprint>,
}

impl ProxyContext {
    /// The verdict as the request log stores it.
    fn shield_verdict(&self) -> Option<String> {
        self.shield
            .as_ref()
            .map(|verdict| verdict.action.as_str().to_string())
    }

    /// The JA4 as the request log stores it.
    fn ja4(&self) -> Option<String> {
        self.client_fingerprint
            .and_then(|fingerprint| fingerprint.ja4)
            .map(|ja4| ja4.to_string())
    }

    fn ja4_source(&self) -> Option<String> {
        self.client_fingerprint
            .map(|fingerprint| fingerprint.source.as_str().to_string())
    }

    fn shield_reasons(&self) -> Vec<String> {
        self.shield
            .as_ref()
            .map(temps_shield::Verdict::reason_strings)
            .unwrap_or_default()
    }
}

/// Main load balancer proxy implementation using traits
pub struct LoadBalancer {
    upstream_resolver: Arc<dyn UpstreamResolver>,
    proxy_log_handle: ProxyLogBatchHandle,
    tracking_handle: TrackingBatchHandle,
    project_context_resolver: Arc<dyn ProjectContextResolver>,
    cookie_config: CookieConfig,
    crypto: Arc<temps_core::CookieCrypto>,
    /// `TempsLogin` exchange grants already traded on this proxy, so each
    /// is used once (ADR-055 §11). Taken once per sign-in, never per request.
    consumed_exchanges: Arc<crate::ingress::ConsumedExchanges>,
    /// Pending ACME HTTP-01 challenges, so the unauthenticated challenge
    /// path never queries the database per request.
    acme_challenges: Arc<AcmeChallengeCache>,
    /// Token for the local preview gateway, from the cached settings.
    preview_gateway_token: crate::service::preview_gateway_token::PreviewGatewayToken,
    config_service: Arc<temps_config::ConfigService>,
    ip_access_control_service: Arc<IpAccessControlService>,
    /// Per-project/environment IP restriction. Defaults to an always-allow
    /// gate (`temps_core::OpenIpGate`) via the write-once `ProjectIpGateSlot`
    /// handoff — see the module doc on `temps_core::project_ip_gate` for why
    /// this is a plain required field here rather than an `Option`: a gate
    /// value always exists, whether or not a plugin claimed the slot.
    project_ip_gate: Arc<dyn temps_core::ProjectIpGate>,
    request_policy_gate: Arc<dyn temps_core::RequestPolicyGate>,
    /// Shield (ADR 066): the decision engine, the reputation lists (the
    /// built-in baseline until a loader swaps in a compiled snapshot), the
    /// bypass tokens and the event sink. Decides without locks or I/O.
    shield: crate::shield::ProxyShield,
    /// Project edge rules (ADR-089), compiled in memory and kept current by
    /// the gate's own poll. `None` enforces no edge rules.
    edge_rules: Option<Arc<crate::edge_rules::EdgeRuleGate>>,
    /// GeoIP for edge rules that test country, continent, ASN or hosting
    /// network. Consulted only for a project whose rules need it. `None`
    /// when no GeoIP database is installed: such conditions never match.
    edge_geo: Option<Arc<temps_geo::GeoIpService>>,
    /// In-memory snapshot of domains that have a TLS certificate. Used by the
    /// HTTP→HTTPS redirect check instead of issuing 2 DB queries per request.
    /// Refreshed every 30 s by `CertHostCache::run_refresh_loop`. See WS3.
    cert_host_cache: Arc<CertHostCache>,
    disable_https_redirect: bool,
    trust_loopback_forwarded_ip: Arc<AtomicBool>,
    /// The header a trusted loopback proxy sends the visitor's JA4 in
    /// (ADR 066, D4). Refreshed from the platform settings; read lock-free.
    loopback_ja4_header: Arc<arc_swap::ArcSwapOption<axum::http::HeaderName>>,
    on_demand_manager: Option<Arc<OnDemandManager>>,
    /// On-demand HTTP-01 TLS cert manager (ADR-018). When set, the port-80
    /// `request_filter` reads its in-process state cache (NO DB hit) to serve a
    /// human-readable 503 for hostnames currently `pending`/`issuing`/`failed`,
    /// so the end user gets a signal on :80 while the TLS handshake keeps
    /// fast-failing (Option B). `None` keeps the legacy behavior.
    on_demand_cert_manager: Option<Arc<crate::on_demand_cert::OnDemandCertManager>>,
    /// Proxy in-memory route table, used by the on-demand HTTP UX path (ADR §5)
    /// to classify a host as ephemeral (`cert_eligible == false`) and to derive
    /// the stable per-environment redirect target for `redirect_to_env` mode.
    /// O(1) in-memory lookup, no DB I/O. `None` disables the ephemeral-host
    /// `deployment_url_mode` handling (serves HTTP as before).
    route_table: Option<Arc<temps_routes::CachedPeerTable>>,
    file_store: Option<Arc<dyn temps_file_store::FileStore>>,
    /// Object-store-backed static-site file serving. `None` for every
    /// existing self-hosted install (the default, unset
    /// `TEMPS_STATIC_STORAGE_BACKEND`): `serve_static_file` then behaves
    /// exactly as before this field existed, reading straight off local disk.
    /// `Some` only when an operator opts into `TEMPS_STATIC_STORAGE_BACKEND=s3`,
    /// in which case this is the same S3-backed, byte-cached `FileStore` as
    /// `file_store` above (see `temps-proxy/src/server.rs`) — the two fields
    /// exist separately because they address disjoint key namespaces (path
    /// keys for static-site files here, content-hash keys for CAS blobs in
    /// `file_store`), not because they can point at different backends.
    static_object_store: Option<Arc<dyn temps_file_store::FileStore>>,
    /// In-memory moka cache for `static_asset_cache` DB lookups. Keyed on
    /// `(project_id, environment_id, deployment_id, url_path)`; values are `Option<content_hash>` so that
    /// **negative results (no row found) are cached too** — the miss case is
    /// the common path for container deployments where most assets are served
    /// by upstream, not the fallback store. TTL 60 s, max ~50 k entries. See
    /// `service/static_asset_lookup.rs` and WS4 in IMPLEMENTATION_PLAN.md.
    static_asset_lookup: Arc<crate::service::static_asset_lookup::StaticAssetLookup>,
    preview_auth_limiter: Arc<PreviewAuthLimiter>,
    /// Per-project/environment concurrent-connection cap enforcement. See
    /// issue #646 and `crate::connection_limiter`.
    connection_limiter: Arc<crate::connection_limiter::ConnectionLimiter>,
    /// Per-client-IP rate limits and rate-limit blacklists from the instance
    /// and project security settings. See issue #1288 and
    /// `crate::rate_limiter`.
    rate_limiter: Arc<crate::rate_limiter::RateLimiter>,
    /// In-memory moka cache for sandbox preview lookups. Keyed by sandbox
    /// hex suffix; values are `PreviewSandboxLookup` (both `Protected` and
    /// `NotFound` are cached). TTL 30 s. See `preview_auth.rs` and WS6 in
    /// IMPLEMENTATION_PLAN.md.
    ///
    /// Password rotation invalidates preview cookies cryptographically (the
    /// cookie binds a SHA-256 fingerprint of the argon2 PHC hash), so a
    /// ≤30 s stale cache window only affects brand-new login attempts
    /// immediately after a password change — existing cookies are unaffected.
    sandbox_lookup_cache: Arc<SandboxLookupCache>,
    /// Shared admin-gate snapshot. When set and non-noop, requests for
    /// hosts that aren't in the route table are gated before falling back
    /// to the console — see `request_filter`. When `None`, gate enforcement
    /// is skipped entirely (used by older test harnesses).
    admin_gate: Option<temps_core::admin_gate::AdminGateHandle>,
    /// Lock-free hot-path request counters (status classes + duration
    /// histogram). Updated on every completed/failed request; drained by the
    /// background `ProxyMetricsSampler`, never read on the request path.
    proxy_metrics: Arc<crate::metrics::ProxyMetrics>,
    upstream_failures: temps_core::log_transitions::KeyedFailureLatch<(i32, i32, Option<String>)>,
    /// Startup state of the in-process console (`temps serve`). When set and
    /// the console failed to start, console-bound requests get a status page
    /// naming the cause instead of the generic 503 — see
    /// `crate::console_unavailable`. `None` in split topology (`temps proxy`),
    /// where the console is another process this one cannot observe.
    console_unavailable: Option<Arc<crate::console_unavailable::ConsoleUnavailableResponder>>,
    /// The signed-in gate (ADR 065 D8). `None` only in test harnesses that
    /// never serve a signed-in address; `setup_proxy_server` always sets it.
    gate: Option<Arc<crate::handler::gate::ProxyGate>>,
}

/// Status recorded in proxy logs when the client disconnected before a
/// response could be sent (the nginx convention).
const CLIENT_CLOSED_REQUEST: u16 = 499;

/// Answer a sandbox preview request the proxy refuses (an address outside
/// the allow-list, a Temps login that cannot be completed, a password
/// sandbox with no password): `status` with a plain-text `body` that says
/// what is wrong and how to fix it. Never cached, never forwarded.
async fn write_preview_refusal(
    session: &mut PingoraSession,
    ctx: &mut ProxyContext,
    status: StatusCode,
    body: String,
    routing_status: &'static str,
) -> Result<bool> {
    let mut response = ResponseHeader::build(status, None)?;
    response.insert_header("Cache-Control", "no-store")?;
    response.insert_header("Referrer-Policy", "no-referrer")?;
    response.insert_header("X-Request-ID", &ctx.request_id)?;
    response.insert_header("Content-Type", "text/plain; charset=utf-8")?;
    session
        .write_response_header(Box::new(response), false)
        .await?;
    session
        .write_response_body(Some(Bytes::from(body)), true)
        .await?;
    ctx.routing_status = routing_status.to_string();
    Ok(true)
}

/// Answer a request for a sandbox preview that does not exist.
async fn write_preview_not_found(
    session: &mut PingoraSession,
    ctx: &mut ProxyContext,
) -> Result<bool> {
    write_preview_refusal(
        session,
        ctx,
        StatusCode::NOT_FOUND,
        "Sandbox preview not found\n".to_string(),
        "preview_not_found",
    )
    .await
}

/// Send the visitor to `location` (a sanitised path on this host) with a
/// 303, never cached.
async fn write_preview_redirect(
    session: &mut PingoraSession,
    ctx: &mut ProxyContext,
    location: &str,
    routing_status: &'static str,
) -> Result<bool> {
    let mut response = ResponseHeader::build(StatusCode::SEE_OTHER, None)?;
    response.insert_header("Location", location)?;
    response.insert_header("Cache-Control", "no-store")?;
    response.insert_header("Referrer-Policy", "no-referrer")?;
    response.insert_header("X-Request-ID", &ctx.request_id)?;
    session
        .write_response_header(Box::new(response), true)
        .await?;
    ctx.routing_status = routing_status.to_string();
    Ok(true)
}

/// The `next` query parameter of a preview request, sanitised to a path on
/// this host.
fn preview_next_from_query(query: Option<&str>) -> String {
    let next = query
        .and_then(|qs| {
            url::form_urlencoded::parse(qs.as_bytes())
                .find(|(k, _)| k == "next")
                .map(|(_, v)| v.into_owned())
        })
        .unwrap_or_else(|| "/".to_string());
    sanitize_next(&next)
}

/// When a request Shield would challenge but that cannot show the page (an
/// API call, a POST) may retry (ADR 066, D7): by then the visitor may have
/// loaded a page and earned a clearance.
const SHIELD_RETRY_AFTER_SECS: u32 = 60;

/// The problem body of that 429. It says what to do, never why.
const SHIELD_CHALLENGE_PROBLEM: &str = r#"{"type":"about:blank","title":"Challenge required","status":429,"detail":"This site checks visitors before serving them. Open it in a browser to pass the check, then retry."}"#;

/// A challenge over plain HTTP (426): the clearance cookie needs TLS.
const SHIELD_HTTPS_REQUIRED_PAGE: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>HTTPS required</title>
<style>body{font-family:system-ui,-apple-system,sans-serif;display:flex;align-items:center;justify-content:center;min-height:100vh;margin:0;background:#0a0a0a;color:#e5e5e5}div{max-width:480px;padding:2rem;text-align:center}p{color:#a3a3a3}</style>
</head>
<body><div><h1>HTTPS required</h1><p>This site checks visitors over a secure connection. Open it with <strong>https://</strong> instead of http://.</p></div></body>
</html>
"#;

/// Entity tags of the solver files, from their content.
static SOLVER_JS_ETAG: std::sync::LazyLock<String> =
    std::sync::LazyLock::new(|| content_etag(temps_shield::assets::SOLVER_JS.as_bytes()));
static SOLVER_WASM_ETAG: std::sync::LazyLock<String> =
    std::sync::LazyLock::new(|| content_etag(temps_shield::assets::SOLVER_WASM));

fn content_etag(content: &[u8]) -> String {
    use sha2::Digest;
    let digest = sha2::Sha256::digest(content);
    format!("\"{}\"", hex::encode(&digest[..12]))
}

/// How the client reached this ingress, which stands in for the JA4 in the
/// visitor identity when there is none (ADR 066, D3).
fn shield_source(ctx: &ProxyContext) -> &'static str {
    ctx.client_fingerprint
        .map(|fingerprint| fingerprint.source.as_str())
        .unwrap_or(temps_shield::ja4::Ja4Source::Direct.as_str())
}

/// Project outages share a key; unscoped routes use their actual upstream.
fn upstream_failure_key(
    project_id: Option<i32>,
    environment_id: Option<i32>,
    upstream: Option<&str>,
    host: &str,
) -> (i32, i32, Option<String>) {
    (
        project_id.unwrap_or(0),
        environment_id.unwrap_or(0),
        project_id
            .is_none()
            .then(|| upstream.unwrap_or(host).to_owned()),
    )
}

/// Who caused a proxy failure, which decides how loudly it is logged.
///
/// Only a failure inside Temps is an ERROR. A client that disconnected
/// mid-request is routine (a browser cancelling a fetch), and an upstream
/// that refuses or drops the connection is the deployed application being
/// down, which is the operator's to see at WARN, not a Temps fault.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProxyFailureKind {
    ClientGone,
    Upstream,
    Internal,
}

impl ProxyFailureKind {
    fn of(error: &Error) -> Self {
        use pingora::{ErrorSource, ErrorType};
        match error.esource() {
            ErrorSource::Downstream => match error.etype() {
                ErrorType::ConnectionClosed | ErrorType::ReadError | ErrorType::WriteError => {
                    Self::ClientGone
                }
                _ => Self::Internal,
            },
            ErrorSource::Upstream => Self::Upstream,
            ErrorSource::Internal | ErrorSource::Unset => Self::Internal,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::ClientGone => "client disconnected",
            Self::Upstream => "upstream unavailable",
            Self::Internal => "internal",
        }
    }
}

impl LoadBalancer {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        upstream_resolver: Arc<dyn UpstreamResolver>,
        proxy_log_handle: ProxyLogBatchHandle,
        tracking_handle: TrackingBatchHandle,
        project_context_resolver: Arc<dyn ProjectContextResolver>,
        crypto: Arc<temps_core::CookieCrypto>,
        db: Arc<DbConnection>,
        config_service: Arc<temps_config::ConfigService>,
        ip_access_control_service: Arc<IpAccessControlService>,
        project_ip_gate: Arc<dyn temps_core::ProjectIpGate>,
        challenge_gate: Arc<temps_shield::ChallengeGate>,
        cert_host_cache: Arc<CertHostCache>,
        disable_https_redirect: bool,
    ) -> Self {
        Self {
            upstream_resolver,
            proxy_log_handle,
            tracking_handle,
            project_context_resolver,
            cookie_config: CookieConfig::default(),
            crypto,
            consumed_exchanges: Arc::new(crate::ingress::ConsumedExchanges::default()),
            static_asset_lookup: Arc::new(
                crate::service::static_asset_lookup::StaticAssetLookup::new(Arc::clone(&db)),
            ),
            sandbox_lookup_cache: Arc::new(SandboxLookupCache::new(Arc::clone(&db))),
            acme_challenges: Arc::new(AcmeChallengeCache::new(db)),
            preview_gateway_token: Default::default(),
            config_service,
            ip_access_control_service,
            project_ip_gate,
            request_policy_gate: Arc::new(temps_core::OpenRequestPolicyGate),
            shield: crate::shield::ProxyShield::new(challenge_gate),
            edge_rules: None,
            edge_geo: None,
            cert_host_cache,
            disable_https_redirect,
            trust_loopback_forwarded_ip: Arc::new(AtomicBool::new(false)),
            loopback_ja4_header: Arc::new(arc_swap::ArcSwapOption::empty()),
            on_demand_manager: None,
            on_demand_cert_manager: None,
            route_table: None,
            file_store: None,
            static_object_store: None,
            preview_auth_limiter: Arc::new(PreviewAuthLimiter::new()),
            connection_limiter: Arc::new(crate::connection_limiter::ConnectionLimiter::new()),
            rate_limiter: Arc::new(crate::rate_limiter::RateLimiter::new()),
            admin_gate: None,
            proxy_metrics: Arc::new(crate::metrics::ProxyMetrics::default()),
            upstream_failures: Default::default(),
            console_unavailable: None,
            gate: None,
        }
    }

    /// Enforce the signed-in gate (ADR 065 D8) for hosts the route table
    /// marks `signed_in`. Hosts without a gate are not affected.
    pub fn with_gate(mut self, gate: Arc<crate::handler::gate::ProxyGate>) -> Self {
        self.gate = Some(gate);
        self
    }

    /// Run the signed-in gate for this request. `Some(true)` when the gate
    /// answered it; `None` when the host has no gate or the visitor is
    /// admitted (identity headers set, gate cookies removed).
    async fn enforce_gate(
        &self,
        session: &mut PingoraSession,
        ctx: &mut ProxyContext,
    ) -> Result<Option<bool>> {
        let Some(gate) = self.gate.as_ref() else {
            return Ok(None);
        };
        let hostname = crate::handler::gate::gate_hostname(&ctx.host);
        let Some(policy) = gate.policy_for(&hostname) else {
            return Ok(None);
        };
        ctx.gated_host = true;
        // ACME HTTP-01 first, as on the agent: a signed-in address must still
        // get and renew its certificate, and Let's Encrypt never signs in.
        // Only a challenge stored for this host is answered; any other path
        // under the prefix meets the gate, so the prefix is no way past it.
        if ctx.path.starts_with(ACME_HTTP01_PREFIX) {
            if let Some(key_authorization) = self
                .handle_acme_http_challenge(&ctx.host, &ctx.path)
                .await?
            {
                let body = Bytes::from(key_authorization);
                let mut resp = ResponseHeader::build(200, None)?;
                resp.insert_header("Content-Type", "text/plain")?;
                resp.insert_header("Cache-Control", "no-cache")?;
                resp.insert_header("X-Request-ID", &ctx.request_id)?;
                resp.insert_header("Content-Length", body.len().to_string())?;
                session.write_response_header(Box::new(resp), false).await?;
                session.write_response_body(Some(body), true).await?;
                ctx.routing_status = "acme_challenge".to_string();
                return Ok(Some(true));
            }
        }
        // An operator who turned the HTTPS redirect off terminates TLS in
        // front of this listener; sending the visitor to HTTPS again would
        // loop. Either way the gate runs: this only picks the redirect.
        let secure = self.is_https_request(session) || self.disable_https_redirect;
        let outcome = {
            let header = session.req_header();
            let request = temps_core::gate::GateRequest {
                hostname: &hostname,
                method: &header.method,
                path: header.uri.path(),
                query: header.uri.query(),
                headers: &header.headers,
                secure,
                // Tunnels end at a worker's tunnel origin, never here.
                via_tunnel: false,
            };
            gate.evaluate(&policy, &request, temps_core::gate::unix_now())
                .await
        };
        match outcome {
            crate::handler::gate::GateOutcome::Admit(claims) => {
                crate::handler::gate::admit(session.req_header_mut(), &claims)?;
                ctx.gate_identity = Some(claims);
                Ok(None)
            }
            crate::handler::gate::GateOutcome::Respond(answer, routing_status) => {
                let (response, body) = crate::handler::gate::render(&answer, &ctx.request_id)?;
                session
                    .write_response_header(Box::new(response), body.is_none())
                    .await?;
                if let Some(body) = body {
                    session.write_response_body(Some(body), true).await?;
                }
                ctx.routing_status = routing_status.to_string();
                Ok(Some(true))
            }
        }
    }

    /// Wire the in-process console's startup state so console-bound requests
    /// explain a failed (or still running) console startup. `console_address`
    /// must be the address the upstream resolver forwards console traffic to.
    pub fn with_console_startup_state(
        mut self,
        state: Arc<temps_core::console_startup::ConsoleStartupState>,
        console_address: &str,
        console_ingress_address: Option<&str>,
    ) -> Self {
        self.console_unavailable = Some(Arc::new(
            crate::console_unavailable::ConsoleUnavailableResponder::new(state, console_address)
                .with_ingress_address(console_ingress_address),
        ));
        self
    }

    /// Enforce project edge rules (ADR-089) from `gate`, with `geo` for the
    /// rules that test where a visitor comes from.
    pub fn with_edge_rules(
        mut self,
        gate: Arc<crate::edge_rules::EdgeRuleGate>,
        geo: Option<Arc<temps_geo::GeoIpService>>,
    ) -> Self {
        self.edge_rules = Some(gate);
        self.edge_geo = geo;
        self
    }

    /// Check requests against `lists`, which a loader keeps current, instead
    /// of the built-in baseline alone.
    pub fn with_shield_lists(mut self, lists: Arc<crate::shield::ShieldLists>) -> Self {
        self.shield.lists = lists;
        self
    }

    /// Accept the bypass tokens `tokens` holds, which a loader keeps
    /// current. Without it no token is accepted.
    pub fn with_shield_bypass_tokens(mut self, tokens: Arc<crate::shield::BypassTokens>) -> Self {
        self.shield.bypass = tokens;
        self
    }

    /// Record Shield events in `events` instead of discarding them.
    pub fn with_shield_events(mut self, events: Arc<dyn temps_shield::EventSink>) -> Self {
        self.shield.events = events;
        self
    }

    /// Talk to the node's netwatch (ADR 066 D12): its bans join the lists
    /// as this node's own, and every request Shield sees is reported to it.
    pub fn with_netwatch(mut self, netwatch: temps_shield::netwatch::Netwatch) -> Self {
        self.shield.local = netwatch.local;
        self.shield.netwatch = netwatch.sender;
        self
    }

    /// What this node adds to the lists on its own (netwatch, ADR 066 D12).
    pub fn with_shield_local_index(
        mut self,
        local: Arc<arc_swap::ArcSwapOption<temps_shield::Index>>,
    ) -> Self {
        self.shield.local = local;
        self
    }

    /// Verify claimed crawlers with `verifier` instead of the system DNS.
    pub fn with_crawler_verifier(
        mut self,
        verifier: Arc<temps_shield::crawler_check::CrawlerVerifier>,
    ) -> Self {
        self.shield.set_crawler_verifier(verifier);
        self
    }

    pub fn with_request_policy_gate(
        mut self,
        gate: Arc<dyn temps_core::RequestPolicyGate>,
    ) -> Self {
        self.request_policy_gate = gate;
        self
    }

    /// Record into a counter set shared with other components (the proxy-log
    /// batch writer reports its retries and drops there) instead of this
    /// instance's private one.
    pub fn with_proxy_metrics(mut self, metrics: Arc<crate::metrics::ProxyMetrics>) -> Self {
        self.proxy_metrics = metrics;
        self
    }

    /// Handle to the hot-path metrics counters, for the background sampler.
    /// The returned `Arc` shares the counters this instance records into.
    pub fn proxy_metrics(&self) -> Arc<crate::metrics::ProxyMetrics> {
        Arc::clone(&self.proxy_metrics)
    }

    /// Wire the shared admin-gate handle. When set, `request_filter` keeps
    /// requests the gate denies away from the console: they are sent to the
    /// public-only listener when one exists (ADR-051), and answered with a
    /// 404 otherwise. `/api/_temps/*` is never gated because public ingest
    /// must work from any host.
    pub fn with_admin_gate(mut self, handle: temps_core::admin_gate::AdminGateHandle) -> Self {
        self.admin_gate = Some(handle);
        self
    }

    /// Enable forwarded client IPs only for an explicitly configured local proxy.
    pub fn with_trust_loopback_forwarded_ip(mut self, enabled: Arc<AtomicBool>) -> Self {
        self.trust_loopback_forwarded_ip = enabled;
        self
    }

    /// The header a trusted loopback proxy sends the visitor's JA4 in.
    pub fn with_loopback_ja4_header(
        mut self,
        header: Arc<arc_swap::ArcSwapOption<axum::http::HeaderName>>,
    ) -> Self {
        self.loopback_ja4_header = header;
        self
    }

    /// Set the file store for path-keyed static asset serving.
    pub fn with_file_store(mut self, store: Arc<dyn temps_file_store::FileStore>) -> Self {
        self.file_store = Some(store);
        self
    }

    /// Enable object-store-backed static-site serving (`serve_static_file`
    /// reads through this instead of local disk). Only called when
    /// `TEMPS_STATIC_STORAGE_BACKEND=s3` resolves to an S3 backend — leaving
    /// this unset keeps every existing self-hosted install on the disk-only
    /// path. See the field doc on `static_object_store`.
    pub fn with_static_object_store(mut self, store: Arc<dyn temps_file_store::FileStore>) -> Self {
        self.static_object_store = Some(store);
        self
    }

    /// Set the on-demand manager for scale-to-zero wake-on-request.
    pub fn with_on_demand_manager(mut self, manager: Arc<OnDemandManager>) -> Self {
        self.on_demand_manager = Some(manager);
        self
    }

    /// Wire the on-demand HTTP-01 TLS cert manager (ADR-018) and the route table
    /// so the port-80 `request_filter` can surface a human-readable 503 while a
    /// cert is provisioning/failed, and can honor `deployment_url_mode` for
    /// ephemeral per-deployment hostnames. Both are required for the on-demand
    /// HTTP UX; the route table classifies hosts (ephemeral vs stable) and
    /// derives the `redirect_to_env` target without a DB lookup.
    pub fn with_on_demand_cert_manager(
        mut self,
        manager: Arc<crate::on_demand_cert::OnDemandCertManager>,
        route_table: Arc<temps_routes::CachedPeerTable>,
    ) -> Self {
        self.on_demand_cert_manager = Some(manager);
        self.route_table = Some(route_table);
        self
    }

    // Test-only accessors for integration tests
    #[cfg(test)]
    pub fn upstream_resolver(&self) -> &Arc<dyn UpstreamResolver> {
        &self.upstream_resolver
    }

    #[cfg(test)]
    pub fn project_context_resolver(&self) -> &Arc<dyn ProjectContextResolver> {
        &self.project_context_resolver
    }

    /// Pull the W3C `traceparent` trace_id (the 32-hex-char `<trace-id>` field)
    /// from a request header map. Returns `None` when the header is missing,
    /// malformed, or carries the all-zero invalid trace_id reserved by the
    /// spec. Stamped onto `proxy_logs.trace_id` so the unified Observe view
    /// can join the request row to its child spans, runtime logs, and any
    /// captured exceptions.
    fn extract_traceparent_trace_id(
        headers: Option<&std::collections::HashMap<String, String>>,
    ) -> Option<String> {
        let headers = headers?;
        let raw = headers
            .get("traceparent")
            .or_else(|| headers.get("Traceparent"))
            .or_else(|| headers.get("TRACEPARENT"))?;

        // traceparent: "<version>-<trace-id>-<parent-id>-<flags>"
        let mut parts = raw.split('-');
        let _version = parts.next()?;
        let trace_id = parts.next()?;

        if trace_id.len() != 32
            || !trace_id.chars().all(|c| c.is_ascii_hexdigit())
            || trace_id.chars().all(|c| c == '0')
        {
            return None;
        }

        Some(trace_id.to_ascii_lowercase())
    }

    /// Answer a failed console-bound request with the console's startup
    /// status instead of the generic 503, when the console in this process
    /// failed to start or is still starting. Returns `None` to fall through
    /// to the generic failure handling (application upstreams, a running
    /// console that errored, split topology, a client that already left).
    async fn serve_console_status_page(
        &self,
        session: &mut PingoraSession,
        e: &Error,
        ctx: &mut ProxyContext,
    ) -> Option<FailToProxy> {
        use temps_core::console_startup::ConsolePhase;

        let responder = self.console_unavailable.as_ref()?;
        if !ctx
            .upstream_host
            .as_deref()
            .is_some_and(|host| responder.is_console_address(host))
        {
            return None;
        }
        if ProxyFailureKind::of(e) == ProxyFailureKind::ClientGone {
            return None;
        }

        let req = session.req_header();
        let format = crate::console_unavailable::ResponseFormat::for_request(
            req.uri.path(),
            req.headers.get("accept").and_then(|v| v.to_str().ok()),
        );
        let (body, routing_status, retry_after) = match responder.phase() {
            ConsolePhase::Failed => {
                let client_ip = ctx
                    .ip_address
                    .as_deref()
                    .and_then(|ip| ip.parse::<std::net::IpAddr>().ok());
                let gate = self.admin_gate.as_ref().map(|gate| gate.current());
                let detailed = crate::console_unavailable::client_may_see_details(
                    client_ip,
                    &ctx.host,
                    gate.as_deref(),
                );
                (
                    responder.failure_body(detailed, format)?,
                    "console_startup_failed",
                    None,
                )
            }
            ConsolePhase::Starting if is_connect_failure(e) => (
                crate::console_unavailable::ConsoleUnavailableResponder::starting_body(format),
                "console_starting",
                Some(crate::console_unavailable::CONSOLE_STARTING_RETRY_AFTER_SECS),
            ),
            ConsolePhase::Starting | ConsolePhase::Running => return None,
        };

        // The cause was logged once, at ERROR, when startup failed; one line
        // per request here would only repeat it.
        debug!(
            request_id = %ctx.request_id,
            host = %ctx.host,
            path = %ctx.path,
            routing_status,
            "serving console status page"
        );
        ctx.error_message = Some(e.to_string());
        ctx.routing_status = routing_status.to_string();

        let error_code = 503;
        let written = async {
            let mut header = ResponseHeader::build(StatusCode::SERVICE_UNAVAILABLE, None)?;
            header.insert_header(header::SERVER, &SERVER_NAME[..])?;
            header.insert_header(header::CACHE_CONTROL, "no-store")?;
            header.insert_header(header::CONTENT_TYPE, format.content_type())?;
            header.insert_header(header::CONTENT_LENGTH, body.len().to_string())?;
            header.insert_header("X-Content-Type-Options", "nosniff")?;
            header.insert_header("X-Request-ID", &ctx.request_id)?;
            if let Some(seconds) = retry_after {
                header.insert_header(header::RETRY_AFTER, seconds.to_string())?;
            }
            session
                .write_response_header(Box::new(header), false)
                .await?;
            session
                .write_response_body(Some(body.clone()), true)
                .await?;
            Ok::<(), Box<Error>>(())
        }
        .await;
        if let Err(write_error) = written {
            debug!(
                request_id = %ctx.request_id,
                "Failed to write console status page: {:?}",
                write_error
            );
        }
        self.log_failed_request(ctx, error_code, Some(body.len() as i64));

        Some(FailToProxy {
            error_code,
            can_reuse_downstream: false,
        })
    }

    /// Whether this request is for the dedicated public ingress hostname
    /// and a public-only listener exists to pin it to. Lock-free and
    /// allocation-free: one `Option` check and one snapshot comparison.
    fn is_public_ingress_request(&self, host: &str) -> bool {
        self.upstream_resolver.console_ingress_address().is_some()
            && self.upstream_resolver.is_public_ingress_host(host)
    }

    /// Decide where a request that is about to fall through to the console
    /// may go, given the admin gate. Pure, so the three-way decision of
    /// ADR-051 D4 is testable without a live session.
    fn console_fall_through(
        config: &temps_core::admin_gate::AdminGateConfig,
        client_ip: std::net::IpAddr,
        host: &str,
        has_public_only_listener: bool,
    ) -> ConsoleFallThrough {
        if config.would_allow(client_ip, Some(host)) {
            ConsoleFallThrough::Console
        } else if has_public_only_listener {
            ConsoleFallThrough::PublicSurfaceOnly
        } else {
            ConsoleFallThrough::NotFound
        }
    }

    /// Decide whether the admin gate should be consulted for this request.
    ///
    /// The gate is only meaningful when it's non-noop, the request isn't a
    /// workspace/sandbox preview (those carry their own auth), and the path
    /// isn't a public temps ingest endpoint (`/api/_temps/*` must work from
    /// any host; the resolver sends it to the public-only listener when one
    /// exists, so skipping the gate exposes nothing else). Even when this
    /// returns `true`, the caller
    /// must still consult `has_route_for_host` first so legitimate project
    /// traffic is never gated — only console fall-throughs are.
    fn should_consult_admin_gate(
        config: &temps_core::admin_gate::AdminGateConfig,
        path: &str,
        is_preview: bool,
    ) -> bool {
        !config.is_noop() && !is_preview && !path.starts_with(ROUTE_PREFIX_TEMPS)
    }

    /// Record a request that failed before an upstream response was sent
    /// (skipping static assets, like successful requests).
    fn log_failed_request(
        &self,
        ctx: &ProxyContext,
        status_code: u16,
        response_size_bytes: Option<i64>,
    ) {
        if Self::should_log_request(&ctx.path) {
            // Prefer bytes actually received from the client (see log_request);
            // fall back to Content-Length if the body never reached the filter.
            let request_size = if ctx.client_body_bytes_received > 0 {
                Some(ctx.client_body_bytes_received as i64)
            } else {
                ctx.request_headers
                    .as_ref()
                    .and_then(|h| h.get("content-length"))
                    .and_then(|v| v.parse::<i64>().ok())
            };

            let (request_source, is_system_request) =
                Self::traffic_classification(&ctx.path, &ctx.user_agent);
            let proxy_log_request = CreateProxyLogRequest {
                method: ctx.method.clone(),
                path: ctx.path.clone(),
                query_string: None,
                host: ctx.host.clone(),
                status_code: status_code as i16,
                response_time_ms: Some(ctx.start_time.elapsed().as_millis() as i32),
                request_source: request_source.to_string(),
                is_system_request,
                routing_status: ctx.routing_status.clone(),
                project_id: ctx.project.as_ref().map(|p| p.id),
                environment_id: ctx.environment.as_ref().map(|e| e.id),
                deployment_id: ctx.deployment.as_ref().map(|d| d.id),
                session_id: None,
                visitor_id: None,
                visitor_uuid: ctx.visitor_id.clone(),
                session_uuid: ctx.session_id.clone(),
                container_id: None,
                upstream_host: None,
                error_message: ctx.error_message.clone(),
                client_ip: ctx.ip_address.clone(),
                user_agent: Some(ctx.user_agent.clone()),
                referrer: ctx.referrer.clone(),
                request_id: ctx.request_id.clone(),
                ip_geolocation_id: None,
                browser: None,
                browser_version: None,
                operating_system: None,
                device_type: None,
                is_bot: None,
                bot_name: None,
                request_size_bytes: request_size,
                response_size_bytes,
                cache_status: None,
                request_headers: ctx
                    .request_headers
                    .as_ref()
                    .and_then(|h| serde_json::to_value(h).ok()),
                response_headers: ctx
                    .response_headers
                    .as_ref()
                    .and_then(|h| serde_json::to_value(h).ok()),
                trace_id: Self::extract_traceparent_trace_id(ctx.request_headers.as_ref()),
                error_group_id: None,
                shield_verdict: ctx.shield_verdict(),
                shield_reasons: ctx.shield_reasons(),
                ja4: ctx.ja4(),
                ja4_source: ctx.ja4_source(),
            };

            // Non-blocking enqueue; shed with rate-limited accounting when full.
            self.proxy_log_handle.send_or_drop(proxy_log_request);
        }
    }

    /// Check if a request should be logged to proxy_logs based on path
    fn should_log_request(path: &str) -> bool {
        if LOG_STATIC_ASSETS {
            return true;
        }

        // Common static file extensions to skip
        let static_extensions = [
            ".js", ".mjs", ".cjs", ".css", ".scss", ".sass", ".less", ".map", ".png", ".jpg",
            ".jpeg", ".gif", ".svg", ".ico", ".webp", ".avif", ".woff", ".woff2", ".ttf", ".eot",
            ".otf", ".mp4", ".webm", ".ogg", ".mp3", ".wav", ".pdf", ".zip", ".tar", ".gz",
        ];

        let path_lower = path.to_lowercase();
        !static_extensions
            .iter()
            .any(|ext| path_lower.ends_with(ext))
    }

    fn traffic_classification(path: &str, user_agent: &str) -> (&'static str, bool) {
        if user_agent.starts_with("Temps-Status-Monitor/") {
            ("temps_monitor", true)
        } else if path.starts_with(ROUTE_PREFIX_TEMPS) {
            ("proxy", true)
        } else {
            ("proxy", false)
        }
    }

    fn request_authority(&self, session: &PingoraSession) -> Result<PublicAuthority> {
        let raw_authority = if let Some(host) = session.req_header().headers.get("host") {
            host.to_str()
                .map_err(|_| Error::new_str("Invalid host header encoding"))?
        } else if let Some(authority) = session.req_header().uri.authority() {
            // HTTP/2 carries the public authority in the request URI.
            authority.as_str()
        } else {
            return Err(Error::new_str("Missing Host or :authority header"));
        };

        parse_public_authority(raw_authority)
            .ok_or_else(|| Error::new_str("Invalid Host or :authority header"))
    }

    fn get_host_header(&self, session: &PingoraSession) -> Result<String> {
        Ok(self.request_authority(session)?.host)
    }

    /// Extract the TLS fingerprint, version and cipher of the connection.
    ///
    /// The fingerprint describes only the TLS session, so it must not
    /// contain the full address. Shield binds a clearance to the JA4
    /// resolved here instead, with the client's /24 or /64 and the
    /// User-Agent (ADR 066, D3).
    fn extract_tls_info(&self, session: &PingoraSession, ctx: &mut ProxyContext) {
        // The JA4 this ingress computed for the connection, if it ended TLS.
        let mut direct_ja4 = None;
        // Access SSL digest from the downstream session's digest
        // digest() returns Option<&Digest>, and Digest contains ssl_digest: Option<Arc<SslDigest>>
        if let Some(digest) = session.downstream_session.digest() {
            if let Some(ssl_digest) = &digest.ssl_digest {
                if let Some(fingerprint) =
                    tls_fingerprint::compute_tls_fingerprint_from_arc(ssl_digest)
                {
                    debug!(
                        "Extracted fingerprint: {} for request_id={}",
                        fingerprint, ctx.request_id
                    );
                    ctx.tls_fingerprint = Some(fingerprint);
                }

                // Extract TLS version and cipher for logging
                // version/cipher are Cow<'static, str> in Pingora 0.8.0
                ctx.tls_version = Some(ssl_digest.version.to_string());
                ctx.tls_cipher = Some(ssl_digest.cipher.to_string());

                // Extract SNI hostname from SslDigestExtension (Pingora 0.8.0)
                // The SNI is captured during the TLS handshake via handshake_complete_callback
                // in server.rs and stored as TlsExtensionData in the SslDigest extension.
                if let Some(ext_data) = ssl_digest
                    .extension
                    .get::<crate::server::TlsExtensionData>()
                {
                    debug!(
                        "SNI hostname from TLS extension: {} for request_id={}",
                        ext_data.sni_hostname, ctx.request_id
                    );
                    direct_ja4 = ext_data.ja4;
                }

                let version: &str = ssl_digest.version.as_ref();
                let cipher: &str = ssl_digest.cipher.as_ref();
                debug!(
                    "TLS connection: {} with cipher {} for request_id={}",
                    version, cipher, ctx.request_id
                );
            } else {
                debug!(
                    "No SSL digest available in Digest for request_id={}",
                    ctx.request_id
                );
            }
        } else {
            debug!(
                "No digest available from downstream_session for request_id={}",
                ctx.request_id
            );
        }

        ctx.client_fingerprint = Some(resolve_client_fingerprint(
            session
                .client_addr()
                .and_then(|address| address.as_inet())
                .map(|address| address.ip()),
            &session.req_header().headers,
            self.trust_loopback_forwarded_ip.load(Ordering::Relaxed),
            self.loopback_ja4_header.load().as_deref(),
            direct_ja4,
        ));
    }

    async fn finalize_response(
        &self,
        session: &mut PingoraSession,
        upstream_response: &mut ResponseHeader,
        ctx: &mut ProxyContext,
    ) -> Result<()> {
        upstream_response.insert_header("X-Request-ID", &ctx.request_id)?;

        // Public status pages (#1412) carry their own strict headers (no-store,
        // CSP `default-src 'none'`, DENY framing); a global preset must not
        // loosen them. On a custom status hostname nothing else is added
        // either: no tracking cookies.
        if ctx.status_page_host {
            return Ok(());
        }
        if !ctx.path.starts_with("/api/status/") {
            // Apply security headers from project settings or global config
            self.apply_security_headers(upstream_response, ctx.project.as_deref())
                .await?;
        }

        // Set visitor and session cookies
        self.set_tracking_cookies(session, upstream_response, ctx)
            .await?;

        // Capture response headers before logging
        let response_headers: HashMap<String, String> = upstream_response
            .headers
            .iter()
            .filter_map(|(k, v)| v.to_str().ok().map(|val| (k.to_string(), val.to_string())))
            .collect();
        ctx.response_headers = Some(response_headers);

        self.log_request(session, upstream_response, ctx).await?;
        self.add_response_timing(upstream_response, ctx)?;

        Ok(())
    }

    fn is_https_request(&self, session: &PingoraSession) -> bool {
        // SECURITY (SEC-12): do NOT trust a client-supplied `X-Forwarded-Proto`.
        // Pingora is the edge TLS terminator, so the only authoritative signal
        // is whether this downstream connection actually has a TLS digest.
        // Trusting the header let a client on the plain-HTTP listener spoof
        // `https`, influencing Secure-cookie attributes and the proto we forward
        // upstream.
        self.is_tls_connection(session)
    }

    /// Check if the connection is a TLS connection by checking for SSL digest
    fn is_tls_connection(&self, session: &PingoraSession) -> bool {
        session
            .downstream_session
            .digest()
            .and_then(|d| d.ssl_digest.as_ref())
            .is_some()
    }

    /// Serve a request for a status page's custom hostname (#1412).
    ///
    /// Allowed, in order:
    /// 1. ACME HTTP-01 challenges for the hostname, so ownership can be proven
    ///    and the certificate renewed.
    /// 2. `GET`/`HEAD` of `/` and `/summary.json` while the page is published
    ///    and the hostname verified: an HTTPS upgrade on plain HTTP, otherwise
    ///    a rewrite to the page's slug route, pinned to the public-only
    ///    listener by `upstream_peer`.
    ///
    /// Everything else gets the status-page 404, byte-identical to the
    /// console's, whatever the reason.
    async fn handle_status_page_host(
        &self,
        session: &mut PingoraSession,
        ctx: &mut ProxyContext,
        route: temps_routes::StatusPageHostRoute,
    ) -> Result<bool> {
        use temps_core::public_status_page::{StatusHostPath, STATUS_HOST_HEADER};

        let host = temps_routes::normalize_route_hostname(&ctx.host).into_owned();
        ctx.routing_status = "status_page_host".to_string();

        if ctx.path.starts_with(ACME_HTTP01_PREFIX) {
            if let Some(key_authorization) =
                self.handle_acme_http_challenge(&host, &ctx.path).await?
            {
                let body = Bytes::from(key_authorization);
                let mut resp = ResponseHeader::build(200, None)?;
                resp.insert_header("Content-Type", "text/plain")?;
                resp.insert_header("Cache-Control", "no-cache")?;
                resp.insert_header("X-Request-ID", &ctx.request_id)?;
                resp.insert_header("Content-Length", body.len().to_string())?;
                session.write_response_header(Box::new(resp), false).await?;
                session.write_response_body(Some(body), true).await?;
                ctx.routing_status = "status_page_host_acme".to_string();
                return Ok(true);
            }
            return self.write_status_page_not_found(session, ctx, false).await;
        }

        let view = StatusHostPath::parse(&ctx.path);
        let method_allowed = matches!(ctx.method.as_str(), "GET" | "HEAD");
        let (Some(view), true, Some(slug)) = (view, method_allowed, route.slug) else {
            return self
                .write_status_page_not_found(session, ctx, ctx.path == "/summary.json")
                .await;
        };

        // Over TLS the certificate the client negotiated and the Host it sent
        // must name the same hostname: a connection made for one status page
        // cannot be steered to another with a forged Host header. The console
        // re-checks the hostname-to-page binding (`STATUS_HOST_HEADER`) too.
        let is_tls = self.is_tls_connection(session);
        if is_tls {
            let sni = Self::tls_sni(session);
            if sni
                .as_deref()
                .map(temps_routes::normalize_route_hostname)
                .as_deref()
                != Some(host.as_str())
            {
                // 421 rather than 404: a browser reusing a connection opened
                // for another name on the same certificate (HTTP/2 connection
                // coalescing) retries on a fresh connection with the right SNI.
                debug!(
                    host = %host,
                    sni = ?sni,
                    "status-page request whose TLS SNI does not match its Host; answering 421"
                );
                return self.write_misdirected(session, ctx).await;
            }
        } else if !self.disable_https_redirect {
            // A verified hostname always has its certificate: upgrade.
            let location = format!("https://{host}{}", ctx.path);
            let resp = https_redirect_response(&location, &ctx.request_id)?;
            ctx.routing_status = "status_page_host_https_redirect".to_string();
            session.write_response_header(Box::new(resp), true).await?;
            return Ok(true);
        }

        if self.upstream_resolver.console_ingress_address().is_none() {
            // Split topology without `--console-ingress-address`: the only
            // other listener is the full console, which this host must never
            // reach. Fail closed.
            warn!(
                host = %host,
                "status-page custom hostname requested but no public ingress listener is configured; answering 404"
            );
            return self
                .write_status_page_not_found(session, ctx, view == StatusHostPath::Summary)
                .await;
        }

        let target = view.console_path(&slug);
        let header = session.req_header_mut();
        header.set_raw_path(target.as_bytes())?;
        // Credentials and caller-chosen forwarding headers never reach the
        // public listener from a custom hostname.
        header.remove_header("cookie");
        header.remove_header("authorization");
        header.remove_header("x-forwarded-host");
        strip_untrusted_client_ip_headers(header, self.loopback_ja4_header.load().as_deref());
        if let Some(ip) = ctx.ip_address.as_deref() {
            header.insert_header("X-Forwarded-For", ip)?;
        } else {
            header.remove_header("x-forwarded-for");
        }
        header.insert_header("X-Forwarded-Proto", if is_tls { "https" } else { "http" })?;
        header.insert_header("Host", host.as_str())?;
        header.insert_header(STATUS_HOST_HEADER, host.as_str())?;

        ctx.path = target;
        ctx.query_string = None;
        ctx.status_page_host = true;
        Ok(false)
    }

    /// SNI captured during the TLS handshake (`TlsExtensionData`), if any.
    fn tls_sni(session: &PingoraSession) -> Option<String> {
        session
            .downstream_session
            .digest()
            .and_then(|d| d.ssl_digest.as_ref())
            .and_then(|d| d.extension.get::<crate::server::TlsExtensionData>())
            .map(|data| data.sni_hostname.clone())
    }

    /// `421 Misdirected Request`: the connection was negotiated for another
    /// hostname. A browser retries on a fresh connection with the right SNI
    /// (e.g. after HTTP/2 connection coalescing across a shared certificate).
    async fn write_misdirected(
        &self,
        session: &mut PingoraSession,
        ctx: &mut ProxyContext,
    ) -> Result<bool> {
        debug!(host = %ctx.host, "request on a status-page TLS connection names another host; answering 421");
        let mut response = ResponseHeader::build(421, None)?;
        response.insert_header("Content-Length", "0")?;
        response.insert_header("Cache-Control", "no-store")?;
        response.insert_header("X-Request-ID", &ctx.request_id)?;
        session
            .write_response_header(Box::new(response), true)
            .await?;
        ctx.routing_status = "status_page_host_misdirected".to_string();
        Ok(true)
    }

    async fn write_status_page_not_found(
        &self,
        session: &mut PingoraSession,
        ctx: &mut ProxyContext,
        json: bool,
    ) -> Result<bool> {
        use temps_core::public_status_page::{NOT_FOUND_HTML, NOT_FOUND_JSON, SECURITY_HEADERS};
        let (body, content_type) = if json {
            (NOT_FOUND_JSON, "application/problem+json")
        } else {
            (NOT_FOUND_HTML, "text/html; charset=utf-8")
        };
        let mut response = ResponseHeader::build(StatusCode::NOT_FOUND, None)?;
        response.insert_header("Content-Type", content_type)?;
        for (name, value) in SECURITY_HEADERS {
            response.insert_header(name, value)?;
        }
        response.insert_header("X-Request-ID", &ctx.request_id)?;
        response.insert_header("Content-Length", body.len().to_string())?;
        let head_only = ctx.method == "HEAD";
        session
            .write_response_header(Box::new(response), head_only)
            .await?;
        if !head_only {
            session
                .write_response_body(Some(Bytes::from_static(body.as_bytes())), true)
                .await?;
        }
        ctx.routing_status = "status_page_host_not_found".to_string();
        Ok(true)
    }

    async fn log_request(
        &self,
        _session: &PingoraSession,
        upstream_response: &ResponseHeader,
        ctx: &mut ProxyContext,
    ) -> Result<()> {
        // Skip logging for internal temps API routes
        if ctx.path.starts_with(ROUTE_PREFIX_TEMPS) {
            return Ok(());
        }

        let status_code = upstream_response.status.as_u16() as i32;

        // Asynchronously log to proxy_logs table via batch writer (skip static assets)
        if Self::should_log_request(&ctx.path) {
            // Request body has already fully streamed through request_body_filter
            // by the time response headers arrive (the client finishes sending
            // before the upstream replies), so the accumulated count is reliable
            // here. Fall back to Content-Length only for bodies that never
            // reached the filter (e.g. HEAD).
            let request_size = if ctx.client_body_bytes_received > 0 {
                Some(ctx.client_body_bytes_received as i64)
            } else {
                ctx.request_headers
                    .as_ref()
                    .and_then(|h| h.get("content-length"))
                    .and_then(|v| v.parse::<i64>().ok())
            };

            // This function runs when response *headers* arrive — the response
            // body hasn't streamed through response_body_filter yet, so
            // upstream_body_bytes_received is always 0 here. Content-Length is
            // the best information available now; the `logging` hook (true
            // end-of-request, after the body has fully streamed) overwrites
            // this with the accumulated byte count before the entry is sent.
            let response_size = ctx
                .response_headers
                .as_ref()
                .and_then(|h| h.get("content-length"))
                .and_then(|v| v.parse::<i64>().ok());

            // Extract cache status from response headers
            let cache_status = ctx
                .response_headers
                .as_ref()
                .and_then(|h| h.get("x-cache").or_else(|| h.get("cf-cache-status")))
                .cloned();

            let (request_source, is_system_request) =
                Self::traffic_classification(&ctx.path, &ctx.user_agent);
            let proxy_log_request = CreateProxyLogRequest {
                method: ctx.method.clone(),
                path: ctx.path.clone(),
                query_string: ctx.query_string.clone(),
                host: ctx.host.clone(),
                status_code: status_code as i16,
                response_time_ms: Some(ctx.start_time.elapsed().as_millis() as i32),
                request_source: request_source.to_string(),
                is_system_request,
                routing_status: ctx.routing_status.clone(),
                project_id: ctx.project.as_ref().map(|p| p.id),
                environment_id: ctx.environment.as_ref().map(|e| e.id),
                deployment_id: ctx.deployment.as_ref().map(|d| d.id),
                session_id: None,
                visitor_id: None,
                visitor_uuid: ctx.visitor_id.clone(),
                session_uuid: ctx.session_id.clone(),
                container_id: ctx.container_id.clone(),
                upstream_host: ctx.upstream_host.clone(),
                error_message: ctx.error_message.clone(),
                client_ip: ctx.ip_address.clone(),
                user_agent: Some(ctx.user_agent.clone()),
                referrer: ctx.referrer.clone(),
                request_id: ctx.request_id.clone(),
                // Batch writer will enrich these fields
                ip_geolocation_id: None,
                browser: None,
                browser_version: None,
                operating_system: None,
                device_type: None,
                is_bot: None,
                bot_name: None,
                request_size_bytes: request_size,
                response_size_bytes: response_size,
                cache_status,
                request_headers: ctx
                    .request_headers
                    .as_ref()
                    .and_then(|h| serde_json::to_value(h).ok()),
                response_headers: ctx
                    .response_headers
                    .as_ref()
                    .and_then(|h| serde_json::to_value(h).ok()),
                trace_id: Self::extract_traceparent_trace_id(ctx.request_headers.as_ref()),
                error_group_id: None,
                shield_verdict: ctx.shield_verdict(),
                shield_reasons: ctx.shield_reasons(),
                ja4: ctx.ja4(),
                ja4_source: ctx.ja4_source(),
            };

            // Stash rather than send: the `logging` hook fires after the
            // response body has fully streamed and patches response_size_bytes
            // with the accurate accumulated count before enqueueing.
            ctx.pending_proxy_log = Some(proxy_log_request);
        }

        Ok(())
    }

    fn add_response_timing(
        &self,
        upstream_response: &mut ResponseHeader,
        ctx: &ProxyContext,
    ) -> Result<()> {
        let duration = ctx.start_time.elapsed();
        info!(
            "[{}] {} {} {} - {}ms - {}",
            ctx.method,
            ctx.host,
            ctx.path,
            upstream_response.status.as_u16(),
            duration.as_millis(),
            ctx.ip_address.clone().unwrap_or_default()
        );
        upstream_response
            .insert_header("X-Response-Time", format!("{}ms", duration.as_millis()))?;
        if let Some(pending_ms) = ctx.upstream_write_pending_time_ms {
            upstream_response
                .insert_header("X-Upstream-Write-Pending", format!("{}ms", pending_ms))?;
        }
        Ok(())
    }

    /// Shield's decision for this request (ADR 066, D1, D6, D7, D11), and
    /// its answer when it acts. `Ok(true)` when Shield answered the request.
    ///
    /// The pass path does no I/O and allocates nothing beyond the clearance
    /// cookie's name in the modes that read it; a claimed crawler that would
    /// be challenged waits at most [`crate::shield::CRAWLER_CHECK_TIMEOUT`]
    /// for its DNS check.
    async fn enforce_shield(
        &self,
        session: &mut PingoraSession,
        ctx: &mut ProxyContext,
        project: &projects::Model,
        environment: &environments::Model,
        client_ip: Option<std::net::IpAddr>,
    ) -> Result<bool> {
        let policy = self.shield.policy(project, environment);
        if policy.mode == temps_shield::ShieldMode::Off {
            return Ok(false);
        }
        let now = temps_shield::unix_now();
        // The clearance cookie is `__Host-` and `Secure`: only a TLS
        // connection can receive one.
        let tls = ctx.tls_fingerprint.is_some() || self.is_tls_connection(session);
        let (decision, netwatch_line) = {
            let header = session.req_header();
            let text = |name: &str| header.headers.get(name).and_then(|v| v.to_str().ok());
            let mut matched = [("", ""); temps_shield::signatures::MATCHED_HEADERS.len()];
            let mut count = 0;
            for name in temps_shield::signatures::MATCHED_HEADERS {
                if let Some(value) = text(name) {
                    matched[count] = (name, value);
                    count += 1;
                }
            }
            // Only the modes that act read a clearance.
            let clearance = if policy.mode.enforces() {
                find_request_cookie(
                    header,
                    &temps_shield::clearance_cookie_name(environment.id),
                    |value| Some(value.to_string()),
                )
            } else {
                None
            };
            let ja4 = ctx
                .client_fingerprint
                .and_then(|fingerprint| fingerprint.ja4);
            let request = temps_shield::Request {
                ip: client_ip,
                ja4: ja4.as_ref().map(|ja4| ja4.as_str()),
                source: shield_source(ctx),
                user_agent: &ctx.user_agent,
                method: header.method.as_str(),
                path: &ctx.path,
                path_and_query: header
                    .uri
                    .path_and_query()
                    .map(|pq| pq.as_str())
                    .unwrap_or(&ctx.path),
                accept: text("accept").unwrap_or_default(),
                headers: &matched[..count],
                clearance: clearance.as_deref(),
                bypass_token: text(temps_shield::bypass::HEADER),
                tls,
            };
            let decision = self.shield.decide(&policy, &request, now).await;
            if decision.is_event() {
                self.shield.submit(crate::shield::decision_event(
                    &decision,
                    &request,
                    &crate::shield::EventContext {
                        project_id: policy.project_id,
                        environment_id: policy.environment_id,
                        request_id: &ctx.request_id,
                        host: &ctx.host,
                    },
                ));
            }
            let line = if self.shield.netwatch.is_enabled() {
                temps_shield::netwatch::RequestLine::new(
                    &request,
                    &ctx.host,
                    0,
                    Some(&decision),
                    chrono::Utc::now().timestamp_millis(),
                )
            } else {
                None
            };
            (decision, line)
        };
        ctx.shield_env = Some(environment.id);
        ctx.netwatch_line = netwatch_line;
        if let Some(temps_shield::Admitted::BypassToken(id)) = decision.admitted {
            self.shield.bypass.record_use(id, now);
        }
        ctx.shield_header = decision.header_verdict();
        let temps_shield::Decision {
            verdict, outcome, ..
        } = decision;
        if verdict.action != temps_shield::Action::Pass {
            ctx.shield = Some(verdict);
        }
        self.answer_shield(session, ctx, outcome, None).await
    }

    /// Writes Shield's answer for `outcome` (D1, D7). `Ok(false)` for a pass.
    /// The response names the verdict in `X-Temps-Shield`, never the reason.
    /// `routing_status_override` replaces the recorded routing status, for an edge
    /// rule's challenge.
    async fn answer_shield(
        &self,
        session: &mut PingoraSession,
        ctx: &mut ProxyContext,
        outcome: temps_shield::Outcome,
        routing_status_override: Option<&'static str>,
    ) -> Result<bool> {
        use temps_shield::Outcome;
        let (status, content_type, body, routing_status) = match outcome {
            Outcome::Pass => return Ok(false),
            Outcome::Block => (
                StatusCode::FORBIDDEN,
                "text/plain; charset=utf-8",
                Bytes::from_static(b"Forbidden\n"),
                "shield_block",
            ),
            // The token is bound to this visitor, so no cache may keep or
            // share the page.
            Outcome::Challenge {
                token, difficulty, ..
            } => (
                StatusCode::FORBIDDEN,
                "text/html; charset=utf-8",
                Bytes::from(crate::shield::challenge_page(
                    &token,
                    difficulty,
                    ctx.ip_address.as_deref().unwrap_or("unknown"),
                )),
                "shield_challenge",
            ),
            Outcome::TooManyRequests => (
                StatusCode::TOO_MANY_REQUESTS,
                "application/problem+json",
                Bytes::from_static(SHIELD_CHALLENGE_PROBLEM.as_bytes()),
                "shield_challenge",
            ),
            Outcome::HttpsRequired => (
                StatusCode::UPGRADE_REQUIRED,
                "text/html; charset=utf-8",
                Bytes::from_static(SHIELD_HTTPS_REQUIRED_PAGE.as_bytes()),
                "shield_https_required",
            ),
            Outcome::Unavailable => (
                StatusCode::SERVICE_UNAVAILABLE,
                "text/plain; charset=utf-8",
                Bytes::from_static(b"Service unavailable\n"),
                "shield_unavailable",
            ),
        };
        let mut response = ResponseHeader::build(status, None)?;
        response.insert_header("Content-Type", content_type)?;
        response.insert_header("Content-Length", body.len().to_string())?;
        response.insert_header("Cache-Control", "no-store")?;
        response.insert_header("X-Request-ID", &ctx.request_id)?;
        if let Some(verdict) = ctx.shield_header {
            response.insert_header(
                temps_shield::RESPONSE_HEADER,
                temps_shield::response_header_value(verdict, &ctx.request_id),
            )?;
        }
        match status {
            StatusCode::TOO_MANY_REQUESTS => {
                response.insert_header("Retry-After", SHIELD_RETRY_AFTER_SECS.to_string())?;
            }
            StatusCode::UPGRADE_REQUIRED => {
                response.insert_header("Upgrade", "TLS/1.2, HTTP/1.1")?;
                response.insert_header("Connection", "Upgrade")?;
            }
            StatusCode::SERVICE_UNAVAILABLE => {
                response.insert_header("Retry-After", "5")?;
            }
            _ => {}
        }
        session
            .write_response_header(Box::new(response), false)
            .await?;
        let length = body.len() as i64;
        session.write_response_body(Some(body), true).await?;
        ctx.routing_status = routing_status_override
            .unwrap_or(routing_status)
            .to_string();
        // Requests the proxy answers itself never reach `log_request`; this
        // one is recorded with its verdict (D11).
        self.log_failed_request(ctx, status.as_u16(), Some(length));
        Ok(true)
    }

    /// Answers the challenge page's verify POST (ADR 066): reads the
    /// solution, has the engine check it against the visitor resolved for
    /// this connection (counting a failure towards a harder next challenge,
    /// D6), and on success sets the clearance cookie for the environment the
    /// signed token names. That environment is not taken from how this
    /// request was routed: on a host with path routes (ADR 050) the verify
    /// path resolves to the host's own environment, not the routed one the
    /// challenge was served for.
    async fn answer_challenge_verify(
        &self,
        session: &mut PingoraSession,
        ctx: &mut ProxyContext,
        project: &projects::Model,
        environment: &environments::Model,
        client_ip: Option<std::net::IpAddr>,
    ) -> Result<bool> {
        let mut body = Vec::new();
        let mut too_large = false;
        if ctx.method == "POST" {
            while let Some(chunk) = session.read_request_body().await? {
                if body.len() + chunk.len() > crate::shield::MAX_VERIFY_BODY {
                    too_large = true;
                    break;
                }
                body.extend_from_slice(&chunk);
            }
        }

        let (status, set_cookie, payload) = if ctx.method != "POST" {
            (
                StatusCode::METHOD_NOT_ALLOWED,
                None,
                r#"{"ok":false,"error":"method"}"#.to_string(),
            )
        } else if too_large {
            (
                StatusCode::PAYLOAD_TOO_LARGE,
                None,
                r#"{"ok":false,"error":"too_large"}"#.to_string(),
            )
        } else {
            let ja4 = ctx
                .client_fingerprint
                .and_then(|fingerprint| fingerprint.ja4);
            let request = temps_shield::Request {
                ip: client_ip,
                ja4: ja4.as_ref().map(|ja4| ja4.as_str()),
                source: shield_source(ctx),
                user_agent: &ctx.user_agent,
                method: &ctx.method,
                path: &ctx.path,
                path_and_query: &ctx.path,
                ..temps_shield::Request::default()
            };
            let mode = crate::shield::ProxyShield::mode(project, environment);
            let context = crate::shield::EventContext {
                project_id: project.id,
                environment_id: environment.id,
                request_id: &ctx.request_id,
                host: &ctx.host,
            };
            match self
                .shield
                .engine
                .verify(&body, &request, temps_shield::unix_now())
            {
                temps_shield::VerifyOutcome::Cleared {
                    env_id,
                    set_cookie,
                    solve_ms,
                } => {
                    debug!(
                        environment_id = env_id,
                        ?solve_ms,
                        "Shield: challenge solved"
                    );
                    self.shield.submit(crate::shield::verify_event(
                        Ok(solve_ms),
                        mode,
                        &request,
                        &crate::shield::EventContext {
                            environment_id: env_id,
                            ..context
                        },
                    ));
                    (
                        StatusCode::OK,
                        Some(set_cookie),
                        r#"{"ok":true}"#.to_string(),
                    )
                }
                temps_shield::VerifyOutcome::Rejected(error) => {
                    debug!(%error, "Shield: challenge rejected");
                    let (status, code) = temps_shield::rejection_response(error);
                    self.shield.submit(crate::shield::verify_event(
                        Err(code),
                        mode,
                        &request,
                        &context,
                    ));
                    (
                        StatusCode::from_u16(status).unwrap_or(StatusCode::FORBIDDEN),
                        None,
                        format!(r#"{{"ok":false,"error":"{code}"}}"#),
                    )
                }
                temps_shield::VerifyOutcome::BadRequest => (
                    StatusCode::BAD_REQUEST,
                    None,
                    r#"{"ok":false,"error":"bad_request"}"#.to_string(),
                ),
            }
        };

        let cleared = set_cookie.is_some();
        let mut response = ResponseHeader::build(status, None)?;
        response.insert_header("Content-Type", "application/json")?;
        response.insert_header("Cache-Control", "no-store")?;
        response.insert_header("X-Request-ID", &ctx.request_id)?;
        if status == StatusCode::METHOD_NOT_ALLOWED {
            response.insert_header("Allow", "POST")?;
        }
        if let Some(set_cookie) = set_cookie {
            response.insert_header("Set-Cookie", set_cookie)?;
        }
        session
            .write_response_header(Box::new(response), false)
            .await?;
        session
            .write_response_body(Some(Bytes::from(payload)), true)
            .await?;
        ctx.routing_status = if cleared {
            "challenge_passed"
        } else {
            "challenge_failed"
        }
        .to_string();
        Ok(true)
    }

    /// Serves the challenge page's solver (ADR 066, I1) from the copy built
    /// into `temps-shield`. `Ok(false)` for any other path.
    ///
    /// The paths are not versioned, so the files are cached for an hour and
    /// revalidated by their hash rather than kept for good: a new release
    /// may ship a new solver under the same names.
    async fn serve_solver_asset(
        &self,
        session: &mut PingoraSession,
        ctx: &mut ProxyContext,
    ) -> Result<bool> {
        use temps_shield::assets;
        let (content_type, content, etag): (_, &'static [u8], &str) =
            if ctx.path == assets::SOLVER_JS_PATH {
                (
                    "application/javascript; charset=utf-8",
                    assets::SOLVER_JS.as_bytes(),
                    &SOLVER_JS_ETAG,
                )
            } else if ctx.path == assets::SOLVER_WASM_PATH {
                ("application/wasm", assets::SOLVER_WASM, &SOLVER_WASM_ETAG)
            } else {
                return Ok(false);
            };
        let not_modified = session
            .req_header()
            .headers
            .get(header::IF_NONE_MATCH)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|tags| tags.split(',').any(|tag| tag.trim() == etag));
        let status = if not_modified {
            StatusCode::NOT_MODIFIED
        } else {
            StatusCode::OK
        };
        let mut resp = ResponseHeader::build(status, None)?;
        resp.insert_header(header::CONTENT_TYPE, content_type)?;
        resp.insert_header(header::CACHE_CONTROL, "public, max-age=3600")?;
        resp.insert_header(header::ETAG, etag)?;
        resp.insert_header("X-Content-Type-Options", "nosniff")?;
        resp.insert_header("X-Request-ID", &ctx.request_id)?;
        if not_modified {
            session.write_response_header(Box::new(resp), true).await?;
        } else {
            resp.insert_header(header::CONTENT_LENGTH, content.len().to_string())?;
            session.write_response_header(Box::new(resp), false).await?;
            session
                .write_response_body(Some(Bytes::from_static(content)), true)
                .await?;
        }
        ctx.routing_status = "captcha_wasm".to_string();
        Ok(true)
    }
}

const SERVICE_UNAVAILABLE_PAGE: &str = concat!(
    "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>Service Unavailable</title>",
    "<style>body{font-family:-apple-system,BlinkMacSystemFont,sans-serif;display:flex;",
    "justify-content:center;align-items:center;min-height:100vh;margin:0;background:#0a0a0a;",
    "color:#e5e5e5}div{text-align:center;max-width:480px;padding:2rem}h1{font-size:1.5rem;",
    "margin:0 0 .5rem}p{color:#a3a3a3;margin:.5rem 0;font-size:.9rem}</style></head>",
    "<body><div><h1>Service Unavailable</h1>",
    "<p>This application is temporarily unable to handle requests.</p>",
    "<p style=\"color:#737373;font-size:.8rem\">If you are the site owner, check that your deployment is running.</p>",
    "</div></body></html>"
);

/// A workspace preview is not a deployment: when the proxy cannot reach it,
/// what is down is the preview gateway (or, for a sandbox on a worker, the
/// relay to it), which Temps sets up and keeps retrying on its own. A dev
/// server that is not listening is answered by the gateway itself, so it
/// never lands here.
const PREVIEW_UNAVAILABLE_PAGE: &str = concat!(
    "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>Preview Unavailable</title>",
    "<style>body{font-family:-apple-system,BlinkMacSystemFont,sans-serif;display:flex;",
    "justify-content:center;align-items:center;min-height:100vh;margin:0;background:#0a0a0a;",
    "color:#e5e5e5}div{text-align:center;max-width:480px;padding:2rem}h1{font-size:1.5rem;",
    "margin:0 0 .5rem}p{color:#a3a3a3;margin:.5rem 0;font-size:.9rem}</style></head>",
    "<body><div><h1>Preview Unavailable</h1>",
    "<p>The preview gateway that serves workspace previews is not reachable right now. ",
    "Temps keeps trying to start it; reload in a minute.</p>",
    "<p style=\"color:#737373;font-size:.8rem\">If this persists, an administrator can see why ",
    "and restart it in the Temps console under AI runtime settings &rarr; Preview Gateway ",
    "(<code>/agent-sandbox/preview</code>).</p>",
    "</div></body></html>"
);

/// The 503 page for a request the proxy could not deliver.
fn unavailable_page(preview: bool) -> &'static str {
    if preview {
        PREVIEW_UNAVAILABLE_PAGE
    } else {
        SERVICE_UNAVAILABLE_PAGE
    }
}

/// Resolve the client IP for a session from the TCP peer. CDN client-IP
/// headers require a verified edge peer; local forwarded headers require an
/// explicit opt-in and a loopback peer.
///
/// Security invariant: the *peer address* (not any header) determines which
/// CDN, if any, is trusted. The operator must configure an opted-in local
/// proxy to overwrite or safely append its observed client address. Headers
/// must parse as a bare `IpAddr`; anything else falls back to the peer.
///
/// Chain:
/// 1. Opted-in loopback peer → honor the rightmost `X-Forwarded-For`, or
///    `X-Real-IP` when XFF is absent (see `client_ip`). The local proxy must
///    overwrite or append the connection address; malformed headers fall back
///    to the peer.
/// 2. Cloudflare peer → honor `CF-Connecting-IP` (see `cloudflare_ips`).
/// 3. Bunny CDN peer → honor `X-Real-IP` (see `bunny_ips`).
/// 4. All other peers → use peer address directly.
///
/// Returns `None` for non-inet peers (unix sockets) so callers keep their own
/// fallback. Using `as_inet()` (not string-splitting on `:`) keeps IPv6 peers
/// intact — `[2001:db8::1]:443` must resolve to `2001:db8::1`, not a mangled
/// prefix.
///
/// Bunny's refresher bootstrap is intentionally triggered here
/// unconditionally, on every call, regardless of whether this peer matched
/// Cloudflare, Bunny, or neither — see the long rationale in the
/// `bunny_ips` module doc comment. In short: Cloudflare's CIDR seed is
/// complete enough that gating its refresher on a prior `is_cloudflare`
/// match still self-bootstraps correctly (left unchanged here), but Bunny's
/// individual-IP seed is deliberately sparse and will almost never match a
/// real edge on a fresh deployment, so gating its trigger the same way
/// would deadlock forever. `ensure_refresh_started` is a cheap idempotent
/// no-op (one atomic load/compare-exchange) after the first successful
/// call in the process, so calling it unconditionally here is within the
/// hot-path budget.
fn resolve_session_client_ip(
    session: &PingoraSession,
    trust_loopback_forwarded_ip: bool,
) -> Option<String> {
    let peer = session.client_addr()?.as_inet()?.ip();
    let headers = &session.req_header().headers;

    crate::bunny_ips::BUNNY_TRUST.ensure_refresh_started();

    if let Some(client_ip) =
        crate::client_ip::resolve_loopback_client_ip(peer, headers, trust_loopback_forwarded_ip)
    {
        return Some(client_ip.to_string());
    }

    // --- Cloudflare: check peer first, then header ---
    if crate::cloudflare_ips::CLOUDFLARE_TRUST.is_cloudflare(peer) {
        let cf_connecting_ip = headers
            .get("cf-connecting-ip")
            .and_then(|v| v.to_str().ok());
        return Some(
            crate::cloudflare_ips::CLOUDFLARE_TRUST
                .resolve_client_ip(peer, cf_connecting_ip)
                .to_string(),
        );
    }

    // --- Bunny CDN: check peer first, then header ---
    if crate::bunny_ips::BUNNY_TRUST.is_bunny(peer) {
        let x_real_ip = headers.get("x-real-ip").and_then(|v| v.to_str().ok());
        return Some(
            crate::bunny_ips::BUNNY_TRUST
                .resolve_client_ip(peer, x_real_ip)
                .to_string(),
        );
    }

    // --- Direct connection: use peer address ---
    Some(peer.to_string())
}

const CF_JA4: axum::http::HeaderName = axum::http::HeaderName::from_static("cf-ja4");
const CDN_JA4: axum::http::HeaderName = axum::http::HeaderName::from_static("cdn-ja4");

/// The client's JA4 by the same trust chain as its address (ADR 066 D4).
///
/// A proxy that ends TLS in front of temps shows temps its own ClientHello,
/// so behind one the JA4 can only come from that proxy's header, and a
/// missing header means none: a proxy's JA4 is never taken for the
/// visitor's. Headers are honoured only from the peers that set them, and
/// only in the exact JA4 format; [`strip_untrusted_client_ip_headers`]
/// removes them before the request goes upstream.
///
/// A trusted local proxy's header is the one the platform settings name
/// (`loopback_ja4_header`); with none set, a local proxy gives no JA4.
/// Temps edges and the PROXY protocol come in later steps of phase 3.
fn resolve_client_fingerprint(
    peer: Option<std::net::IpAddr>,
    headers: &axum::http::HeaderMap,
    trust_loopback_forwarded_ip: bool,
    loopback_ja4_header: Option<&axum::http::HeaderName>,
    direct: Option<temps_shield::ja4::Ja4>,
) -> temps_shield::ja4::ClientFingerprint {
    use temps_shield::ja4::{ClientFingerprint, Ja4, Ja4Source};

    let forwarded = |name: &axum::http::HeaderName| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .and_then(Ja4::parse_forwarded)
    };
    let (ja4, source) = match peer {
        Some(peer)
            if crate::client_ip::resolve_loopback_client_ip(
                peer,
                headers,
                trust_loopback_forwarded_ip,
            )
            .is_some() =>
        {
            (loopback_ja4_header.and_then(forwarded), Ja4Source::Local)
        }
        Some(peer) if crate::cloudflare_ips::CLOUDFLARE_TRUST.is_cloudflare(peer) => {
            (forwarded(&CF_JA4), Ja4Source::Cloudflare)
        }
        Some(peer) if crate::bunny_ips::BUNNY_TRUST.is_bunny(peer) => {
            (forwarded(&CDN_JA4), Ja4Source::Bunny)
        }
        Some(_) | None => (direct, Ja4Source::Direct),
    };
    ClientFingerprint { ja4, source }
}

impl LoadBalancer {
    /// Serve a request for a sandbox preview host
    /// (`ws-<hex>-<port>.<preview_domain>`): the password form and its
    /// logout, the Temps-login exchange, then the access check. Returns
    /// `true` when it wrote the response, `false` when the request is
    /// admitted and continues to the preview gateway or worker relay.
    async fn handle_preview_request(
        &self,
        session: &mut PingoraSession,
        ctx: &mut ProxyContext,
        preview_host: PreviewHost,
        settings: &temps_core::AppSettings,
    ) -> Result<bool> {
        // `None` when the proxy could not determine the address: an
        // allow-list then refuses the request rather than guess.
        let client_ip = ctx
            .ip_address
            .as_deref()
            .and_then(|s| s.parse::<std::net::IpAddr>().ok());

        if ctx.path == PREVIEW_LOGIN_PATH && ctx.method == "POST" {
            return self
                .preview_password_login(session, ctx, &preview_host, settings, client_ip)
                .await;
        }
        if ctx.path == PREVIEW_LOGIN_PATH && (ctx.method == "GET" || ctx.method == "HEAD") {
            return self
                .preview_login_form(session, ctx, &preview_host, settings, client_ip)
                .await;
        }
        if ctx.path == PREVIEW_LOGOUT_PATH && ctx.method == "POST" {
            let secure = self.is_tls_connection(session);
            let set_cookie =
                build_logout_cookie_sandbox(&preview_host.hex, &settings.preview_domain, secure);
            let mut response = ResponseHeader::build(303, None)?;
            response.insert_header("Location", "/")?;
            response.append_header("Set-Cookie", &set_cookie)?;
            // Sign out of a Temps-login session on this host as well, with
            // the attributes it was set with (a partitioned cookie is only
            // cleared by a partitioned `Set-Cookie`).
            response.append_header(
                "Set-Cookie",
                format!(
                    "{}=; Path=/; Max-Age=0; HttpOnly{}",
                    crate::ingress::PREVIEW_LOGIN_COOKIE_NAME,
                    crate::ingress::preview_login_cookie_scope(secure)
                ),
            )?;
            response.insert_header("Cache-Control", "no-store")?;
            response.insert_header("X-Request-ID", &ctx.request_id)?;
            session
                .write_response_header(Box::new(response), true)
                .await?;
            ctx.routing_status = "preview_logout".to_string();
            return Ok(true);
        }
        if ctx.path == crate::ingress::GRANT_EXCHANGE_PATH {
            return self
                .preview_login_exchange(session, ctx, &preview_host, client_ip)
                .await;
        }

        // ── Regular preview request: check access ─────────────────────
        // HTTP/2 and some browser cookie stores may emit duplicate cookie
        // names in separate Cookie header fields. Join every field so
        // check_preview_auth can validate every candidate.
        let cookie_values = session
            .req_header()
            .headers
            .get_all("cookie")
            .iter()
            .filter_map(|value| value.to_str().ok())
            .collect::<Vec<_>>();
        let cookie_header = combine_cookie_header_values(cookie_values);

        // Revocations of Temps-login grants come from the route snapshot:
        // one lock-free load, no I/O.
        let snapshot = self
            .route_table
            .as_ref()
            .map(|table| table.ingress_snapshot());
        let no_revocations = temps_workload_ingress::grant::Revocations::default();
        let revocations = snapshot
            .as_deref()
            .map(|snapshot| snapshot.revocations())
            .unwrap_or(&no_revocations);
        let mut host_buffer = [0u8; 256];
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let request = PreviewRequest {
            client_ip,
            cookie_header: cookie_header.as_deref(),
            grants: crate::ingress::GrantFacts {
                master: self.crypto.ingress_grant_master_key(),
                revocations,
                host: crate::ingress::grant_host(&ctx.host, &mut host_buffer),
                now_secs,
            },
        };

        let outcome = check_preview_auth(
            &self.sandbox_lookup_cache,
            &self.crypto,
            &self.preview_auth_limiter,
            preview_host,
            &request,
        )
        .await;

        match outcome {
            PreviewAuthOutcome::Allow { host, on_worker } => {
                info!(
                    target = %host.label(),
                    port = host.port,
                    "preview-auth: allowed"
                );
                ctx.preview_on_worker = on_worker;
                ctx.preview_route = Some(host);
                ctx.routing_status = "preview".to_string();

                // Strip any Authorization header before forwarding so the
                // dev server inside the sandbox never sees upstream secrets
                // that happen to be present.
                let _ = session.req_header_mut().remove_header("authorization");

                // The Temps-login session grant is a credential for this
                // preview: the sandbox never needs it, so it never sees it.
                if let Some(stripped) = cookie_header.as_deref().and_then(|header| {
                    crate::preview_auth::cookie_header_without(
                        header,
                        crate::ingress::PREVIEW_LOGIN_COOKIE_NAME,
                    )
                }) {
                    let _ = session.req_header_mut().remove_header("cookie");
                    if !stripped.is_empty() {
                        session.req_header_mut().insert_header("cookie", stripped)?;
                    }
                }

                // Inject the token of whichever peer serves the sandbox, so
                // it accepts us: the worker preview relay's, or the
                // gateway's shared secret. The secret comes from the cached
                // settings row (reloaded on NOTIFY, so a rotated secret is
                // picked up without a restart), never from a per-request
                // environment read.
                if ctx.preview_on_worker {
                    if let Some(relay) = temps_core::sandbox_preview_relay::worker_preview_relay() {
                        session
                            .req_header_mut()
                            .insert_header("X-Temps-Preview-Token", &relay.token)?;
                    }
                } else if let Some(token) = self
                    .preview_gateway_token
                    .resolve(&settings.preview_gateway.shared_secret)
                {
                    session
                        .req_header_mut()
                        .insert_header("X-Temps-Preview-Token", token.as_str())?;
                }
                // The gateway strips its bearer token once, at the TCP
                // connection boundary. Non-upgrade HTTP traffic must
                // therefore close after this request so neither side can
                // place another token-bearing request on the same
                // authenticated connection. WebSocket upgrades are a single
                // HTTP request followed by non-HTTP frames.
                if !ctx.is_websocket {
                    session
                        .req_header_mut()
                        .insert_header("Connection", "close")?;
                }
                // Continue — upstream_peer will route to the gateway.
                Ok(false)
            }
            PreviewAuthOutcome::LoginRequired { host } => {
                debug!(
                    target = %host.label(),
                    "preview-auth: redirecting to the password form"
                );
                // Build the original path + query to stash as `next`.
                let original = match ctx.query_string.as_deref() {
                    Some(qs) if !qs.is_empty() => format!("{}?{}", ctx.path, qs),
                    _ => ctx.path.clone(),
                };
                let next = sanitize_next(&original);
                let location = format!(
                    "{}?next={}",
                    PREVIEW_LOGIN_PATH,
                    url::form_urlencoded::byte_serialize(next.as_bytes()).collect::<String>()
                );
                write_preview_redirect(session, ctx, &location, "preview_login_required").await
            }
            PreviewAuthOutcome::RateLimited { host } => {
                warn!(
                    target = %host.label(),
                    client_ip = ?client_ip,
                    "preview-auth: rate limited"
                );
                let mut response = ResponseHeader::build(StatusCode::TOO_MANY_REQUESTS, None)?;
                response.insert_header("Retry-After", "60")?;
                response.insert_header("Cache-Control", "no-store")?;
                response.insert_header("X-Request-ID", &ctx.request_id)?;
                response.insert_header("Content-Type", "text/plain; charset=utf-8")?;
                session
                    .write_response_header(Box::new(response), false)
                    .await?;
                session
                    .write_response_body(
                        Some(Bytes::from_static(b"Too many failed attempts\n")),
                        true,
                    )
                    .await?;
                ctx.routing_status = "preview_rate_limited".to_string();
                Ok(true)
            }
            PreviewAuthOutcome::NotFound { host } => {
                debug!(target = %host.label(), "preview-auth: sandbox not found");
                write_preview_not_found(session, ctx).await
            }
            PreviewAuthOutcome::IpDenied { host, client_ip } => {
                info!(
                    target = %host.label(),
                    client_ip = ?client_ip,
                    "preview-auth: client address is outside the sandbox's preview allow-list"
                );
                write_preview_refusal(
                    session,
                    ctx,
                    StatusCode::FORBIDDEN,
                    crate::preview_auth::ip_denied_message(&host.hex, client_ip),
                    "preview_ip_denied",
                )
                .await
            }
            PreviewAuthOutcome::PasswordMissing { host } => {
                warn!(
                    target = %host.label(),
                    "preview-auth: sandbox is in password access with no password stored; refusing"
                );
                write_preview_refusal(
                    session,
                    ctx,
                    StatusCode::FORBIDDEN,
                    crate::preview_auth::password_missing_message(&host.hex),
                    "preview_password_missing",
                )
                .await
            }
            PreviewAuthOutcome::TempsLoginRequired { host, sandbox_id } => {
                let headers = &session.req_header().headers;
                let navigation = crate::preview_auth::is_page_navigation(
                    &ctx.method,
                    headers
                        .get("sec-fetch-mode")
                        .and_then(|value| value.to_str().ok()),
                    headers.get("accept").and_then(|value| value.to_str().ok()),
                    ctx.is_websocket,
                );
                if !navigation {
                    debug!(
                        target = %host.label(),
                        "preview-auth: Temps login required for a request that is not a page load"
                    );
                    return write_preview_refusal(
                        session,
                        ctx,
                        StatusCode::UNAUTHORIZED,
                        crate::preview_auth::temps_login_required_message(&host.hex),
                        "preview_temps_login_required",
                    )
                    .await;
                }
                let next = match ctx.query_string.as_deref() {
                    Some(qs) if !qs.is_empty() => format!("{}?{}", ctx.path, qs),
                    _ => ctx.path.clone(),
                };
                let scheme = if self.is_https_request(session) {
                    crate::ingress::LoginScheme::Https
                } else {
                    crate::ingress::LoginScheme::Http
                };
                // `ctx.host` has no port; the visitor's port comes from the
                // request so the sign-in returns them to it.
                let port = self
                    .request_authority(session)
                    .ok()
                    .and_then(|authority| authority.port);
                let location = settings.external_url.as_deref().and_then(|base| {
                    crate::ingress::login_redirect_url(
                        base,
                        self.crypto.ingress_grant_master_key(),
                        crate::ingress::sandbox_owner(sandbox_id),
                        crate::ingress::LoginSite {
                            scheme,
                            host: &ctx.host,
                            port,
                        },
                        &next,
                        now_secs,
                    )
                });
                match location {
                    Some(location) => {
                        debug!(
                            target = %host.label(),
                            sandbox_id,
                            "preview-auth: sending the visitor to the Temps login"
                        );
                        write_preview_redirect(
                            session,
                            ctx,
                            &location,
                            "preview_temps_login_redirect",
                        )
                        .await
                    }
                    None => {
                        warn!(
                            target = %host.label(),
                            sandbox_id,
                            "Sandbox preview requires a Temps login but the instance has no valid external URL to send the visitor to"
                        );
                        write_preview_refusal(
                            session,
                            ctx,
                            StatusCode::FORBIDDEN,
                            crate::preview_auth::temps_login_unconfigured_message(&host.hex),
                            "preview_temps_login_unconfigured",
                        )
                        .await
                    }
                }
            }
        }
    }

    /// `GET`/`HEAD` [`PREVIEW_LOGIN_PATH`]: the password form, or the share
    /// link bridge, for a `password` sandbox. Any other sandbox has no form:
    /// the visitor is sent on to `next`, where its own gate applies.
    async fn preview_login_form(
        &self,
        session: &mut PingoraSession,
        ctx: &mut ProxyContext,
        preview_host: &PreviewHost,
        settings: &temps_core::AppSettings,
        client_ip: Option<std::net::IpAddr>,
    ) -> Result<bool> {
        let hex = &preview_host.hex;
        let next = preview_next_from_query(ctx.query_string.as_deref());
        let lookup = self.sandbox_lookup_cache.lookup(hex).await;
        let Some(gate) = lookup.gate() else {
            return write_preview_not_found(session, ctx).await;
        };
        if !gate.admits_ip(client_ip) {
            return write_preview_refusal(
                session,
                ctx,
                StatusCode::FORBIDDEN,
                crate::preview_auth::ip_denied_message(hex, client_ip),
                "preview_ip_denied",
            )
            .await;
        }
        if gate.password_hash().is_none() {
            return write_preview_redirect(session, ctx, &next, "preview_login_not_required").await;
        }
        let label = format!("sandbox sbx_{}", hex);

        // A share link carries its grant in the URL fragment, which never
        // reaches this request. The non-secret `grant=1` marker selects a
        // bridge whose JavaScript reads the fragment, clears browser
        // history, and POSTs the grant to the existing verification/cookie
        // path.
        let has_session_grant = ctx
            .query_string
            .as_deref()
            .and_then(|qs| {
                url::form_urlencoded::parse(qs.as_bytes())
                    .find(|(k, _)| k == "grant")
                    .map(|(_, v)| v == "1")
            })
            .unwrap_or(false);

        let html = if has_session_grant {
            generate_preview_bridge_html(&label, &next)
        } else {
            generate_preview_form_html_labeled(
                &label,
                preview_host.port,
                &next,
                false,
                settings.external_url.as_deref(),
            )
        };
        let mut response = ResponseHeader::build(StatusCode::OK, None)?;
        response.insert_header("Content-Type", "text/html; charset=utf-8")?;
        response.insert_header("Cache-Control", "no-store")?;
        response.insert_header("Referrer-Policy", "no-referrer")?;
        response.insert_header("X-Request-ID", &ctx.request_id)?;
        session
            .write_response_header(Box::new(response), false)
            .await?;
        if ctx.method == "GET" {
            session
                .write_response_body(Some(Bytes::from(html)), true)
                .await?;
        } else {
            session.write_response_body(None, true).await?;
        }
        ctx.routing_status = "preview_login_form".to_string();
        Ok(true)
    }

    /// `POST` [`PREVIEW_LOGIN_PATH`]: verify a password (or a share-link
    /// grant) for a `password` sandbox and mint its cookie. The allow-list
    /// is checked first, so an address it refuses cannot guess at all.
    async fn preview_password_login(
        &self,
        session: &mut PingoraSession,
        ctx: &mut ProxyContext,
        preview_host: &PreviewHost,
        settings: &temps_core::AppSettings,
        client_ip: Option<std::net::IpAddr>,
    ) -> Result<bool> {
        let hex = preview_host.hex.as_str();
        // The limiter needs a key; unknown addresses share one bucket.
        let limiter_ip = client_ip.unwrap_or_else(|| std::net::IpAddr::from([127, 0, 0, 1]));
        let lookup = self.sandbox_lookup_cache.lookup(hex).await;
        let Some(gate) = lookup.gate() else {
            return write_preview_not_found(session, ctx).await;
        };
        if !gate.admits_ip(client_ip) {
            return write_preview_refusal(
                session,
                ctx,
                StatusCode::FORBIDDEN,
                crate::preview_auth::ip_denied_message(hex, client_ip),
                "preview_ip_denied",
            )
            .await;
        }
        let Some(stored_hash) = gate.password_hash().map(str::to_string) else {
            // Not a password sandbox — nothing to verify.
            return write_preview_redirect(session, ctx, "/", "preview_login_not_required").await;
        };

        let body = session.read_request_body().await.map_err(|e| {
            error!("preview-auth: failed to read sandbox login body: {}", e);
            e
        })?;
        let body_str = body
            .as_ref()
            .map(|b| String::from_utf8_lossy(b).to_string())
            .unwrap_or_default();
        let params: Vec<(String, String)> = url::form_urlencoded::parse(body_str.as_bytes())
            .into_owned()
            .collect();
        let form_value = |name: &str| {
            params
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.as_str())
        };
        let password = form_value("password").unwrap_or("");
        let session_grant = form_value("session_grant").unwrap_or("");
        let next = sanitize_next(form_value("next").unwrap_or("/"));

        let subject = format!("sbx_{}", hex);
        let now = std::time::SystemTime::now();
        // Grants are bound to the password hash that existed when they were
        // minted. Authenticate the self-contained grant envelope before
        // touching the database so random public input cannot amplify into
        // an uncached lookup. A valid candidate then bypasses the cache so
        // password rotation revokes it immediately rather than after the
        // cache TTL.
        let grant_password_hash = self
            .sandbox_lookup_cache
            .verify_session_grant(&self.crypto, session_grant, hex, now)
            .await;
        let valid_session_grant = grant_password_hash.is_some();

        // A valid platform-session grant is not a password guess and must
        // remain usable even if this IP previously hit the manual-login
        // limiter.
        if !valid_session_grant && self.preview_auth_limiter.is_blocked(limiter_ip, hex) {
            warn!(
                sandbox = %hex,
                client_ip = ?client_ip,
                "preview-auth: sandbox login POST rate limited"
            );
            let mut response = ResponseHeader::build(StatusCode::TOO_MANY_REQUESTS, None)?;
            response.insert_header("Retry-After", "60")?;
            response.insert_header("Cache-Control", "no-store")?;
            response.insert_header("Referrer-Policy", "no-referrer")?;
            response.insert_header("X-Request-ID", &ctx.request_id)?;
            response.insert_header("Content-Type", "text/plain; charset=utf-8")?;
            session
                .write_response_header(Box::new(response), false)
                .await?;
            session
                .write_response_body(
                    Some(Bytes::from_static(b"Too many failed attempts\n")),
                    true,
                )
                .await?;
            ctx.routing_status = "preview_rate_limited".to_string();
            return Ok(true);
        }

        // Password reconciliation can update the sandbox row immediately
        // while this worker still has the previous hash in its short-lived
        // lookup cache. The owner bridge already has the new password, so
        // retry against a fresh row before showing a manual password wall.
        let verified_hash = if let Some(password_hash) = grant_password_hash {
            Some(password_hash)
        } else if verify_argon2(password, &stored_hash) {
            Some(stored_hash)
        } else {
            let fresh = self.sandbox_lookup_cache.lookup_fresh(hex).await;
            let refreshed = fresh
                .password_hash()
                .filter(|password_hash| verify_argon2(password, password_hash))
                .map(str::to_string);
            if refreshed.is_some() {
                debug!(
                    sandbox = %hex,
                    "preview-auth: login succeeded after refreshing stale password hash"
                );
            }
            refreshed
        };

        let Some(stored_hash) = verified_hash else {
            self.preview_auth_limiter.record_failure(limiter_ip, hex);
            debug!(sandbox = %hex, "preview-auth: sandbox login failed (bad password)");
            let label = format!("sandbox sbx_{}", hex);
            let html = generate_preview_form_html_labeled(
                &label,
                preview_host.port,
                &next,
                true,
                settings.external_url.as_deref(),
            );
            let mut response = ResponseHeader::build(StatusCode::UNAUTHORIZED, None)?;
            response.insert_header("Content-Type", "text/html; charset=utf-8")?;
            response.insert_header("Cache-Control", "no-store")?;
            response.insert_header("Referrer-Policy", "no-referrer")?;
            response.insert_header("X-Request-ID", &ctx.request_id)?;
            session
                .write_response_header(Box::new(response), false)
                .await?;
            session
                .write_response_body(Some(Bytes::from(html)), true)
                .await?;
            ctx.routing_status = "preview_login_failed".to_string();
            return Ok(true);
        };

        self.preview_auth_limiter.record_success(limiter_ip, hex);
        let cookie_name = format!("temps_preview_sbx_{}", hex);
        let cookie_values = session
            .req_header()
            .headers
            .get_all("cookie")
            .iter()
            .filter_map(|value| value.to_str().ok())
            .collect::<Vec<_>>();
        let cookie_header = combine_cookie_header_values(cookie_values);
        let cookie_needs_refresh = preview_cookie_needs_refresh(
            &self.crypto,
            cookie_header.as_deref(),
            &cookie_name,
            &subject,
            &stored_hash,
            now,
        );
        // Duplicate names indicate an obsolete cookie scope. Mint the
        // partitioned replacement before expiring the old scopes so cleanup
        // can never delete the only healthy cookie.
        let has_duplicate_candidates = cookie_header
            .as_deref()
            .is_some_and(|header| extract_cookie_values(header, &cookie_name).len() > 1);
        let should_refresh_cookie = cookie_needs_refresh || has_duplicate_candidates;

        let set_cookie = if !should_refresh_cookie {
            None
        } else {
            let Some(cookie_value) =
                encode_preview_cookie_subject(&self.crypto, &subject, &stored_hash, now)
            else {
                error!("preview-auth: failed to encode sandbox preview cookie");
                let mut response = ResponseHeader::build(StatusCode::INTERNAL_SERVER_ERROR, None)?;
                response.insert_header("Cache-Control", "no-store")?;
                response.insert_header("X-Request-ID", &ctx.request_id)?;
                session
                    .write_response_header(Box::new(response), false)
                    .await?;
                session
                    .write_response_body(Some(Bytes::from_static(b"Cookie mint failed\n")), true)
                    .await?;
                ctx.routing_status = "preview_cookie_error".to_string();
                return Ok(true);
            };
            Some(build_set_cookie_sandbox(
                hex,
                &cookie_value,
                &settings.preview_domain,
                self.is_tls_connection(session),
            ))
        };

        info!(
            sandbox = %hex,
            auth_kind = if valid_session_grant { "platform_session" } else { "password" },
            cookie_refreshed = should_refresh_cookie,
            "preview-auth: sandbox login succeeded"
        );
        let mut response = ResponseHeader::build(303, None)?;
        response.insert_header("Location", &next)?;
        if let Some(set_cookie) = &set_cookie {
            response.append_header("Set-Cookie", set_cookie)?;
        }
        // Expire both scopes used by older gateway versions. Chrome can keep
        // an unpartitioned host-only cookie and a parent-domain cookie
        // alongside the fresh CHIPS cookie, sometimes emitting them in
        // separate Cookie header fields.
        if should_refresh_cookie && self.is_tls_connection(session) {
            let unpartitioned_cookie = build_logout_cookie_sandbox_unpartitioned(hex);
            response.append_header("Set-Cookie", &unpartitioned_cookie)?;
            let legacy_cookie = build_logout_cookie_sandbox(hex, &settings.preview_domain, false);
            response.append_header("Set-Cookie", &legacy_cookie)?;
        }
        response.insert_header("Cache-Control", "no-store")?;
        response.insert_header("Referrer-Policy", "no-referrer")?;
        response.insert_header("X-Request-ID", &ctx.request_id)?;
        session
            .write_response_header(Box::new(response), true)
            .await?;
        ctx.routing_status = "preview_login_ok".to_string();
        Ok(true)
    }

    /// [`GRANT_EXCHANGE_PATH`](crate::ingress::GRANT_EXCHANGE_PATH) on a
    /// preview host: the console's redirect back after a Temps login. For a
    /// `temps_login` sandbox, trade the exchange grant for a host-only
    /// session cookie and send the visitor to `next`; any other sandbox is
    /// sent straight to `next`, where its own gate applies.
    ///
    /// Reads the sandbox fresh, so a sandbox switched to `temps_login`
    /// seconds ago signs in without waiting for the lookup cache. Once per
    /// sign-in, never per request.
    async fn preview_login_exchange(
        &self,
        session: &mut PingoraSession,
        ctx: &mut ProxyContext,
        preview_host: &PreviewHost,
        client_ip: Option<std::net::IpAddr>,
    ) -> Result<bool> {
        let hex = preview_host.hex.as_str();
        // The query holds the grant: keep it out of the proxy log, which
        // outlives the grant and is readable by project members.
        let query = ctx.query_string.take();
        let lookup = self.sandbox_lookup_cache.lookup_fresh(hex).await;
        let Some(gate) = lookup.gate() else {
            return write_preview_not_found(session, ctx).await;
        };
        if !gate.admits_ip(client_ip) {
            return write_preview_refusal(
                session,
                ctx,
                StatusCode::FORBIDDEN,
                crate::preview_auth::ip_denied_message(hex, client_ip),
                "preview_ip_denied",
            )
            .await;
        }
        if gate.access != crate::preview_auth::PreviewGateAccess::TempsLogin {
            let next = preview_next_from_query(query.as_deref());
            return write_preview_redirect(session, ctx, &next, "preview_login_not_required").await;
        }

        let secure = self.is_tls_connection(session);
        let snapshot = self
            .route_table
            .as_ref()
            .map(|table| table.ingress_snapshot());
        let no_revocations = temps_workload_ingress::grant::Revocations::default();
        let mut host_buffer = [0u8; 256];
        let grants = crate::ingress::GrantFacts {
            master: self.crypto.ingress_grant_master_key(),
            revocations: snapshot
                .as_deref()
                .map(|snapshot| snapshot.revocations())
                .unwrap_or(&no_revocations),
            host: crate::ingress::grant_host(&ctx.host, &mut host_buffer),
            now_secs: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        };
        let exchange = crate::ingress::exchange_preview_grant(
            gate.sandbox_id,
            &grants,
            &self.consumed_exchanges,
            query.as_deref(),
            secure,
        );
        match exchange {
            Ok(exchange) => {
                info!(
                    sandbox = %hex,
                    sandbox_id = gate.sandbox_id,
                    grant_id = exchange.grant_id,
                    user_id = exchange.user_id,
                    "Temps login grant exchanged for a sandbox preview session"
                );
                let mut response = ResponseHeader::build(StatusCode::SEE_OTHER, None)?;
                response.insert_header("Location", exchange.location)?;
                response.insert_header("Set-Cookie", exchange.set_cookie)?;
                response.insert_header("Cache-Control", "no-store")?;
                response.insert_header("Referrer-Policy", "no-referrer")?;
                response.insert_header("X-Request-ID", &ctx.request_id)?;
                response.insert_header("Content-Type", "text/plain; charset=utf-8")?;
                session
                    .write_response_header(Box::new(response), false)
                    .await?;
                session
                    .write_response_body(Some(Bytes::from_static(b"Signed in.\n")), true)
                    .await?;
                ctx.routing_status = "preview_login_grant_exchanged".to_string();
                Ok(true)
            }
            Err(error) => {
                warn!(
                    sandbox = %hex,
                    sandbox_id = gate.sandbox_id,
                    error = %error,
                    "Refused a Temps login grant exchange for a sandbox preview"
                );
                write_preview_refusal(
                    session,
                    ctx,
                    StatusCode::FORBIDDEN,
                    "This sign-in link is invalid or has expired. Reload the page to sign in again.\n"
                        .to_string(),
                    "preview_login_grant_refused",
                )
                .await
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PublicAuthority {
    host: String,
    forwarded_host: String,
    port: Option<u16>,
}

fn parse_public_authority(raw_authority: &str) -> Option<PublicAuthority> {
    // Userinfo is valid in generic URI authorities but never in an HTTP Host
    // header. Reject it explicitly rather than forwarding ambiguous input.
    if raw_authority.is_empty() || raw_authority.contains('@') {
        return None;
    }

    let has_explicit_port = if raw_authority.starts_with('[') {
        let closing_bracket = raw_authority.find(']')?;
        match &raw_authority[closing_bracket + 1..] {
            "" => false,
            suffix if suffix.starts_with(':') && suffix.len() > 1 => true,
            _ => return None,
        }
    } else if let Some((host, port)) = raw_authority.rsplit_once(':') {
        // HTTP requires IPv6 literals to be bracketed. A single colon is the
        // port separator and must be followed by a valid numeric port.
        if host.contains(':') || port.is_empty() {
            return None;
        }
        true
    } else {
        false
    };

    let authority = raw_authority.parse::<Authority>().ok()?;
    let authority_host = authority.host();
    let host = authority_host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(authority_host);
    if host.is_empty() {
        return None;
    }

    let port = match (has_explicit_port, authority.port_u16()) {
        (false, None) => None,
        (true, Some(port)) if port > 0 => Some(port),
        _ => return None,
    };

    let host = host.to_ascii_lowercase();
    let authority_host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.clone()
    };
    let forwarded_host = match port {
        Some(port) => format!("{authority_host}:{port}"),
        None => authority_host,
    };

    Some(PublicAuthority {
        host,
        forwarded_host,
        port,
    })
}

/// Strip every client-controlled IP/forwarding header from the request that
/// is about to be forwarded upstream to the deployed tenant app.
///
/// Callers must invoke this only *after* `resolve_session_client_ip` has
/// already read whatever CDN header it needed from the original inbound
/// request — the resolved value is then re-emitted as the sole trusted
/// `X-Forwarded-For` (see the call site's comment). At this trust boundary
/// `Forwarded`, `X-Real-IP`, and `CF-Connecting-IP` are all client-supplied:
/// any direct client (bypassing Bunny/Cloudflare entirely) can set them to
/// an arbitrary value. If left in place, a tenant app that itself reads one
/// of these headers (very common, e.g. nginx-era `X-Real-IP` convention)
/// would see the attacker's forged value verbatim instead of the platform's
/// resolved IP, letting an external client forge how its own request
/// appears to the tenant's own IP-based logic (rate limiting, geofencing,
/// abuse detection). Extend this function, not a second call site, if a
/// future CDN adds another raw client-IP header to the trust chain.
///
/// Tailnet identity (`Tailscale-*`, and Temps' own `x-temps-user-login`) is
/// removed too (ADR 065 D3): only a worker's Serve socket ever passes it to
/// an app, so an app that reads `Tailscale-User-Login` cannot be fed one
/// through a public way in. Collecting the names allocates only when the
/// request carries one.
fn strip_untrusted_client_ip_headers(
    request: &mut RequestHeader,
    loopback_ja4_header: Option<&axum::http::HeaderName>,
) {
    request.remove_header("forwarded");
    request.remove_header("x-real-ip");
    request.remove_header("cf-connecting-ip");
    let identity: Vec<header::HeaderName> = request
        .headers
        .keys()
        .filter(|name| temps_core::tailnet::is_identity_header(name.as_str()))
        .cloned()
        .collect();
    for name in identity {
        request.remove_header(&name);
    }
    // The JA4 headers of D4 (ADR 066): the CDNs', the temps edge's and the
    // one a trusted local proxy is configured to send. An app never sees a
    // JA4 header a visitor could have written.
    for name in temps_shield::ja4::FORWARDED_JA4_HEADERS {
        request.remove_header(name);
    }
    if let Some(name) = loopback_ja4_header {
        request.remove_header(name);
    }
}

/// Collapse every `Cookie` field on the request into a single field before it
/// is forwarded upstream.
///
/// HTTP/2 clients may split cookies into separate `cookie` fields for better
/// header compression ("cookie crumbs", RFC 9113 §8.2.3), and browsers do so
/// in practice. Pingora forwards the header map as-is, so an HTTP/1.1
/// upstream would receive several `Cookie:` lines, which RFC 6265 §5.4
/// forbids. Common app servers (PHP, and so WordPress/WooCommerce) then read
/// only the first line and silently lose every other cookie. RFC 9113
/// requires an intermediary to join the crumbs with `"; "` before forwarding
/// to a non-HTTP/2 context; the joined form is equally valid over HTTP/2.
///
/// Values are joined as raw bytes so a crumb that is not valid UTF-8 is kept
/// rather than dropped, and empty crumbs are skipped so the result never
/// contains an empty `; ;` segment.
///
/// Never fails the request: every crumb is already a valid header value and
/// `"; "` is too, so rebuilding the joined value cannot fail in practice. If it
/// ever did, the crumbs are forwarded unchanged (the previous behaviour) and
/// the failure is logged, rather than turning a cookie quirk into a 502.
fn coalesce_cookie_headers(request: &mut RequestHeader) {
    if request
        .headers
        .get_all(header::COOKIE)
        .iter()
        .nth(1)
        .is_none()
    {
        return;
    }

    let crumbs = || {
        request
            .headers
            .get_all(header::COOKIE)
            .iter()
            .map(|value| value.as_bytes().trim_ascii())
            .filter(|crumb| !crumb.is_empty())
    };
    // One exactly-sized buffer that becomes the header value without a copy.
    let mut joined = Vec::with_capacity(crumbs().map(|crumb| crumb.len() + 2).sum());
    for crumb in crumbs() {
        if !joined.is_empty() {
            joined.extend_from_slice(b"; ");
        }
        joined.extend_from_slice(crumb);
    }

    if joined.is_empty() {
        request.remove_header(&header::COOKIE);
        return;
    }
    let result = header::HeaderValue::from_maybe_shared(Bytes::from(joined))
        .map_err(|e| e.to_string())
        .and_then(|value| {
            request
                .insert_header(header::COOKIE, value)
                .map_err(|e| e.to_string())
        });
    if let Err(error) = result {
        warn!(%error, "Could not combine Cookie header fields; forwarding them unchanged");
    }
}

/// Look up a request cookie by name across every `Cookie` field, returning
/// the first value for which `accept` yields `Some`.
///
/// Fields are decoded with `String::from_utf8_lossy`, not `to_str()`: one
/// cookie carrying non-UTF-8 (obs-text) bytes must only garble itself, not
/// hide every other cookie in the same field. That matters because
/// `coalesce_cookie_headers` joins all HTTP/2 crumbs into a single field, and
/// a single-field HTTP/1.1 request can carry such a cookie too. Valid UTF-8
/// (the normal case) is borrowed without allocating.
fn find_request_cookie<T>(
    request: &RequestHeader,
    name: &str,
    mut accept: impl FnMut(&str) -> Option<T>,
) -> Option<T> {
    request
        .headers
        .get_all(header::COOKIE)
        .iter()
        .find_map(|field| {
            let field = String::from_utf8_lossy(field.as_bytes());
            Cookie::split_parse(field.as_ref())
                .filter_map(Result::ok)
                .filter(|cookie| cookie.name() == name)
                .find_map(|cookie| accept(cookie.value()))
        })
}

/// Whether a `Content-Type` value's media type — its "essence", the part
/// before any `;` parameters — is exactly `text/event-stream`.
///
/// Deliberately not a substring match. This gates `ctx.streaming_session`,
/// which excludes a request from the proxy latency histograms, and the value
/// comes from the upstream (i.e. from a tenant's own app). A `contains` check
/// would also accept `text/html; note=text/event-stream`, letting an app
/// classify arbitrary responses as streaming and drop itself out of the
/// operator's latency and alerting series.
///
/// Note this is the right shape only for `Content-Type`, which carries a
/// single media type plus parameters. The request-side `Accept` check stays a
/// substring match because `Accept` is a comma-separated list.
fn is_event_stream_content_type(value: &str) -> bool {
    value
        .split(';')
        .next()
        .is_some_and(|essence| essence.trim().eq_ignore_ascii_case("text/event-stream"))
}

/// Whether a request looks like a browser navigating to a top-level document,
/// as opposed to a data fetch, an embedded subresource, or a generic HTTP
/// client that happens to accept HTML.
///
/// `Sec-Fetch-Dest` is the strong signal and is used whenever it is present:
/// a browser labels a navigation `document` and a `fetch()`/XHR `empty`, so
/// "present and not `document`" is a definitive no. But it is **not** present
/// on every real page view. Per the Fetch Metadata spec, user agents append
/// `Sec-Fetch-*` only for potentially trustworthy URLs — HTTPS and localhost —
/// and Safari only began sending them in 16.4. A self-hosted Temps serving an
/// app over plain HTTP therefore receives no Fetch Metadata at all, and
/// treating that absence as "not a browser" would silently zero out every
/// visitor, session and page view for that operator, with nothing in the UI
/// to explain why.
///
/// So absence falls back to the weaker `Accept` + `text/html` response pair
/// (the caller checks the response content-type). That is spoofable, but the
/// thing it admits is an unauthenticated visitor row — analytics noise, not a
/// trust decision — and a client must still both ask for HTML and be served
/// HTML at a non-asset, non-API path. A browser-issued `fetch()` defaults to
/// `Accept: */*` and is excluded by that alone, which is what keeps framework
/// data fetches out on HTTP origins too.
///
/// Known cost of the fallback: on a plain-HTTP origin an `<iframe>` embed is
/// indistinguishable from a navigation here, because the `Sec-Fetch-Dest:
/// iframe` that would reject it is exactly what the browser withholds — the
/// same goes for prefetch/prerender, whose `Sec-Purpose` is also absent. Those
/// count as page views on HTTP. That is a regression against #700 but not
/// against the behaviour before it, and it is scoped to origins that already
/// get no Fetch Metadata at all. `Upgrade-Insecure-Requests` would tighten the
/// generic-client case (issue #715) but does not separate an iframe from a
/// document either.
fn is_browser_document_request(
    method: &str,
    accept: Option<&str>,
    fetch_destination: Option<&str>,
    upgrade_insecure_requests: Option<&str>,
) -> bool {
    if method != "GET" {
        return false;
    }

    let accepts_html = accept.is_some_and(|value| {
        value.split(',').any(|part| {
            part.split(';')
                .next()
                .is_some_and(|media_type| media_type.trim().eq_ignore_ascii_case("text/html"))
        })
    });
    if !accepts_html {
        return false;
    }

    match fetch_destination {
        Some(destination) => destination.eq_ignore_ascii_case("document"),
        // Fetch Metadata is browser-sent only to potentially trustworthy
        // origins (HTTPS, localhost), so it never arrives on a plain-HTTP
        // deployment. On that path, Accept alone doesn't exclude non-browser
        // clients (curl/wget/scrapers all send a browser-shaped Accept).
        // Upgrade-Insecure-Requests is sent by browsers on top-level
        // navigations to HTTP origins and essentially never by non-browser
        // clients, so require it here. This still doesn't distinguish an
        // <iframe> load from a top-level navigation — both send the header —
        // which Fetch Metadata alone can't fix either.
        None => upgrade_insecure_requests.is_some_and(|value| value.trim() == "1"),
    }
}

/// Whether `error` means the upstream could not be connected to at all
/// (nothing listening yet), as opposed to failing mid-request.
fn is_connect_failure(error: &Error) -> bool {
    use pingora::ErrorType;
    matches!(
        error.etype(),
        ErrorType::ConnectRefused
            | ErrorType::ConnectTimedout
            | ErrorType::ConnectError
            | ErrorType::ConnectNoRoute
            | ErrorType::SocketError
    )
}

/// The workspace preview a request is for, if any.
///
/// A host that is the reserved public ingress hostname is never a preview,
/// even when it has the `ws-<id>-<port>` shape: it must reach the callback
/// routes, not preview authentication or the preview gateway.
///
/// `is_public_ingress_host` is only consulted for a host that parses as a
/// preview, so ordinary requests pay nothing for the check.
fn preview_host_for_request(
    host: &str,
    preview_domain: &str,
    is_public_ingress_host: impl FnOnce() -> bool,
) -> Option<PreviewHost> {
    parse_preview_host(host, preview_domain).filter(|_| !is_public_ingress_host())
}

/// Where a request with no project route may go once the admin gate has
/// been consulted (ADR-051 D4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConsoleFallThrough {
    /// The gate allows the client: the full console (dashboard + admin API).
    Console,
    /// The gate denies the client: only the console's public routes, on the
    /// listener that mounts nothing else.
    PublicSurfaceOnly,
    /// The gate denies the client and there is no public-only listener to
    /// hand it to: the proxy answers 404 itself.
    NotFound,
}

#[async_trait]
impl ProxyHttp for LoadBalancer {
    type CTX = ProxyContext;

    fn new_ctx(&self) -> Self::CTX {
        ProxyContext {
            response_modified: false,
            response_compressed: false,
            upstream_response_headers: None,
            content_type: None,
            buffer: vec![],
            project: None,
            environment: None,
            deployment: None,
            request_id: Uuid::new_v4().to_string(),
            start_time: Instant::now(),
            method: String::new(),
            path: String::new(),
            query_string: None,
            host: String::new(),
            user_agent: String::new(),
            referrer: None,
            ip_address: None,
            visitor_id: None,
            session_id: None,
            is_new_session: false,
            request_headers: None,
            response_headers: None,
            request_visitor_cookie: None,
            request_session_cookie: None,
            is_sse: false,
            is_websocket: false,
            skip_tracking: false,
            routing_status: "pending".to_string(),
            error_message: None,
            upstream_host: None,
            container_id: None,
            container_name: None,
            tls_fingerprint: None,
            tls_version: None,
            tls_cipher: None,
            sni_hostname: None,
            upstream_body_bytes_received: 0,
            client_body_bytes_received: 0,
            pending_proxy_log: None,
            wants_markdown: false,
            markdown_buffer: Vec::new(),
            markdown_fallback_accept_encoding: None,
            upstream_connect_tries: 0,
            upstream_write_pending_time_ms: None,
            upstream_start_time: None,
            upstream_response_time_ms: None,
            preview_route: None,
            preview_on_worker: false,
            path_route: None,
            gated_host: false,
            gate_identity: None,
            shield: None,
            shield_header: None,
            shield_env: None,
            netwatch_line: None,
            client_fingerprint: None,
            public_surface_only: false,
            status_page_host: false,
            streaming_session: false,
            connection_permit: None,
        }
    }

    async fn early_request_filter(
        &self,
        session: &mut PingoraSession,
        ctx: &mut Self::CTX,
    ) -> Result<()> {
        // Extract client IP address FIRST (needed for TLS fingerprinting)
        let client_ip = resolve_session_client_ip(
            session,
            self.trust_loopback_forwarded_ip.load(Ordering::Relaxed),
        )
        .unwrap_or_else(|| "unknown".to_string());
        ctx.ip_address = Some(client_ip.clone());

        // Extract user-agent FIRST (needed for TLS fingerprinting)
        ctx.user_agent = session
            .req_header()
            .headers
            .get("user-agent")
            .map(|h| h.to_str().unwrap_or_default().to_string())
            .unwrap_or_default();

        // Extract TLS fingerprint AFTER IP and user-agent are set
        self.extract_tls_info(session, ctx);

        // Get the request path early to check if this is a CAPTCHA/WASM request
        let path = session.req_header().uri.path();

        // The solver files bypass IP access control: the challenge page needs them.
        let is_wasm_request = path == temps_shield::assets::SOLVER_JS_PATH
            || path == temps_shield::assets::SOLVER_WASM_PATH;

        // Check if IP is blocked - this happens at infrastructure level before any processing
        // WASM routes bypass this check since they're needed for challenge solving
        if !is_wasm_request {
            match self.ip_access_control_service.is_blocked(&client_ip).await {
                Ok(is_blocked) => {
                    if is_blocked {
                        warn!("Blocked request from IP: {}", client_ip);

                        // Return 403 Forbidden immediately
                        let mut response = ResponseHeader::build(StatusCode::FORBIDDEN, None)?;
                        response.insert_header("Content-Type", "text/plain")?;
                        response.insert_header("X-Blocked-Reason", "IP address blocked")?;

                        session
                            .write_response_header(Box::new(response), true)
                            .await?;
                        session
                            .write_response_body(
                                Some(Bytes::from("Access denied: IP address blocked")),
                                true,
                            )
                            .await?;

                        // Return error to stop request processing
                        return Err(Error::because(
                            pingora::ErrorType::HTTPStatus(403),
                            "IP address blocked",
                            pingora_core::Error::new(pingora::ErrorType::HTTPStatus(403)),
                        ));
                    }
                }
                Err(e) => {
                    // Log error but don't block request if IP check fails
                    error!("Failed to check IP access control for {}: {}", client_ip, e);
                }
            }
        }

        // Check if client accepts SSE (Server-Sent Events)
        let accepts_sse = session
            .req_header()
            .headers
            .get("accept")
            .and_then(|v| v.to_str().ok())
            .map(|accept| accept.contains("text/event-stream"))
            .unwrap_or(false);
        let is_chunked = session
            .req_header()
            .headers
            .get("transfer-encoding")
            .and_then(|v| v.to_str().ok())
            .map(|transfer_encoding| transfer_encoding.to_lowercase().contains("chunked"))
            .unwrap_or(false);
        // Check if this is a WebSocket upgrade request
        let is_websocket_upgrade = session
            .req_header()
            .headers
            .get("upgrade")
            .and_then(|v| v.to_str().ok())
            .map(|upgrade| upgrade.to_lowercase().contains("websocket"))
            .unwrap_or(false);

        // Check if the request path suggests it might return streaming data
        let req_path = session.req_header().uri.path().to_string();
        let is_streaming_path = req_path.starts_with("/api/")
            || req_path.contains("/stream")
            || req_path.contains("/events")
            || req_path.contains("/logs")
            || req_path.contains("/webhook");

        // Compressed bodies are streamed with chunked transfer encoding, which
        // an HTTP/1.0 client cannot parse.
        let is_http10_client = matches!(
            session.req_header().version,
            pingora_http::Version::HTTP_09 | pingora_http::Version::HTTP_10
        );
        let compression_disabled = accepts_sse
            || is_websocket_upgrade
            || is_chunked
            || is_streaming_path
            || is_http10_client;
        if compression_disabled {
            // Disable compression for SSE/WebSocket/streaming paths
            // compression requires buffering which breaks streaming responses
            session.upstream_compression.adjust_level(0);
            debug!(
                "Disabling compression for: sse={}, ws={}, chunked={}, path={}",
                accepts_sse, is_websocket_upgrade, is_chunked, req_path
            );

            if accepts_sse {
                ctx.is_sse = true;
                debug!("SSE request detected, disabling compression for streaming");
            }

            if is_websocket_upgrade {
                ctx.is_websocket = true;
                debug!("WebSocket upgrade detected, disabling compression for streaming");
            }

            if is_streaming_path {
                debug!(
                    "Streaming path detected: {}, disabling compression",
                    req_path
                );
            }
        } else {
            // Enable compression for normal requests
            session
                .upstream_compression
                .adjust_level(RESPONSE_COMPRESSION_LEVEL);
        }

        // Detect whether the client prefers a Markdown response.
        // We check for `text/markdown` in the Accept header (case-insensitive substring match
        // is sufficient — quality values and ordering are intentionally ignored here because
        // we only convert when the client explicitly lists `text/markdown`, not as a fallback).
        let wants_markdown = session
            .req_header()
            .headers
            .get("accept")
            .and_then(|v| v.to_str().ok())
            .map(|accept| {
                accept
                    .split(',')
                    .any(|part| part.trim().to_lowercase().starts_with("text/markdown"))
            })
            .unwrap_or(false);

        if wants_markdown {
            // Markdown conversion requires buffering the full body, which is incompatible
            // with streaming responses. Guard here: if early_request_filter already detected
            // SSE or WebSocket we must not buffer.
            if !ctx.is_sse && !ctx.is_websocket {
                ctx.wants_markdown = true;
                if !compression_disabled {
                    ctx.markdown_fallback_accept_encoding = session
                        .req_header()
                        .headers
                        .get("accept-encoding")
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_owned);
                }
                // Don't compress the response ourselves, and ask the upstream
                // not to either, so the body filter receives raw HTML.
                session.upstream_compression.adjust_level(0);
                request_identity_encoding_for_markdown(session.req_header_mut());
                debug!("Client requested text/markdown — enabling HTML-to-Markdown conversion");
            } else {
                debug!(
                    "Client requested text/markdown but response is streaming (SSE/WS) — ignoring"
                );
            }
        }

        Ok(())
    }

    async fn request_filter(
        &self,
        session: &mut PingoraSession,
        ctx: &mut Self::CTX,
    ) -> Result<bool>
    where
        Self::CTX: Send + Sync,
    {
        // Set the started_at time here
        ctx.start_time = Instant::now();

        // Add the request ID to the request headers
        session
            .req_header_mut()
            .insert_header("X-Request-ID", &ctx.request_id)?;

        ctx.host = self.get_host_header(session)?;
        ctx.method = session.req_header().method.to_string();
        ctx.path = session.req_header().uri.path().to_string();
        ctx.query_string = session.req_header().uri.query().map(|q| q.to_string());
        ctx.user_agent = session
            .req_header()
            .headers
            .get("user-agent")
            .map(|h| h.to_str().unwrap_or_default().to_string())
            .unwrap_or_default();

        // Extract client IP address early (needed for Shield)
        if let Some(client_ip) = resolve_session_client_ip(
            session,
            self.trust_loopback_forwarded_ip.load(Ordering::Relaxed),
        ) {
            ctx.ip_address = Some(client_ip);
        }

        // SECURITY: Strip any inbound X-Temps-Demo-Mode header. Demo mode
        // has been removed; clients sending this header should never have it
        // honored by downstream auth middleware.
        let _ = session.req_header_mut().remove_header("X-Temps-Demo-Mode");

        // SECURITY: only the signed-in gate tells an app who is signed in
        // (ADR 065 D8); a client's own identity headers never get through.
        crate::handler::gate::strip_client_identity(session.req_header_mut());

        // SECURITY: only the status-host branch below may tag a request with
        // the custom hostname it was served for (#1412).
        let _ = session
            .req_header_mut()
            .remove_header(temps_core::public_status_page::STATUS_HOST_HEADER);

        // A status page's custom hostname (#1412) is answered here and only
        // here: before previews, wake-on-request, project routing, the admin
        // gate and the console fall-through, so no path or header can steer it
        // anywhere but the page it is attached to.
        if let Some(status_route) = self.upstream_resolver.status_page_host(&ctx.host) {
            return self
                .handle_status_page_host(session, ctx, status_route)
                .await;
        }
        // A TLS connection negotiated for a status hostname may only ever
        // carry requests for that hostname: a different Host must not use it
        // to reach the console or an application.
        if let Some(sni) = Self::tls_sni(session) {
            if self.upstream_resolver.status_page_host(&sni).is_some() {
                return self.write_misdirected(session, ctx).await;
            }
        }

        // Sandbox preview gateway: requests to `ws-<sid>-<port>.<preview_domain>`
        // are checked against the sandbox's preview allow-list, then its
        // access mode: a Temps login (the default), a password form, or
        // public. On success we mark the request as a preview route so
        // `upstream_peer` forwards it to the local gateway or worker relay;
        // otherwise `handle_preview_request` answers it.
        //
        // HTTP Basic auth is NOT supported — see `preview_auth.rs` for
        // rationale.
        //
        // The public ingress hostname (ADR-051) is never a preview, whatever
        // it looks like: it is reserved for the platform's callback routes,
        // and treating a `ws-` shaped one as a sandbox would answer provider
        // callbacks with a login redirect or a 404. The settings API refuses
        // such a hostname; this keeps the guarantee if one is stored anyway.
        if let Ok(settings) = self.config_service.get_settings().await {
            if let Some(preview_host) =
                preview_host_for_request(&ctx.host, &settings.preview_domain, || {
                    self.upstream_resolver.is_public_ingress_host(&ctx.host)
                })
            {
                if self
                    .handle_preview_request(session, ctx, preview_host, &settings)
                    .await?
                {
                    return Ok(true);
                }
            }
        }

        // Signed-in addresses (ADR 065 D8): the gate answers before anything
        // can reach the app, including waking a sleeping environment.
        if let Some(handled) = self.enforce_gate(session, ctx).await? {
            return Ok(handled);
        }

        // On-demand: check if this host maps to a sleeping environment.
        // Sleeping environments are excluded from the route table, so we must
        // check before project context resolution. Wake the environment inline
        // and hold the request until the container is ready and routes are reloaded.
        if let Some(ref on_demand) = self.on_demand_manager {
            let host_without_port = ctx.host.split(':').next().unwrap_or(&ctx.host);
            // An awake route always wins over a sleeping wildcard. Without
            // this check, `api.example.com` could wake an environment behind
            // `*.example.com` even when the exact host belongs to another
            // active project. Both lookups are in-memory.
            if let Some(sleeping_info) =
                should_lookup_sleeping_environment(self.route_table.as_deref(), host_without_port)
                    .then(|| on_demand.get_sleeping_environment(host_without_port))
                    .flatten()
            {
                info!(
                    environment_id = sleeping_info.environment_id,
                    host = %ctx.host,
                    "Request hit sleeping environment, waking inline"
                );

                let env_id = sleeping_info.environment_id;
                let wake_timeout = sleeping_info.wake_timeout_seconds;

                // Reserve a wake slot before parking this request. The wake path
                // can hold the request for several seconds; cap how many requests
                // may be parked here at once so an unauthenticated client that
                // knows a sleeping hostname can't pin proxy worker tasks. Held for
                // the duration of the wake + re-resolve via this guard.
                let _wake_slot = match on_demand.try_acquire_wake_slot() {
                    Some(permit) => permit,
                    None => {
                        warn!(
                            environment_id = env_id,
                            host = %ctx.host,
                            "Wake path at capacity; returning retryable 503 without parking"
                        );
                        let mut response =
                            ResponseHeader::build(StatusCode::SERVICE_UNAVAILABLE, None)?;
                        response.insert_header("Retry-After", "2")?;
                        response.insert_header("Cache-Control", "no-store")?;
                        response.insert_header("X-Request-ID", &ctx.request_id)?;
                        response.insert_header("Content-Type", "application/json")?;
                        // Body carries no environment_id: it has no authorization
                        // significance and the client keys retries off Retry-After,
                        // not the id. This response goes to an unauthenticated
                        // client (a sleeping env has no auth context yet).
                        let body_bytes = Bytes::from_static(
                            br#"{"status":"wake_pending","message":"Environment is starting, please retry"}"#,
                        );
                        session
                            .write_response_header(Box::new(response), false)
                            .await?;
                        session.write_response_body(Some(body_bytes), true).await?;
                        ctx.routing_status = "wake_throttled".to_string();
                        return Ok(true);
                    }
                };

                // Block until the environment is fully awake (containers healthy)
                match on_demand.wake_environment(env_id, wake_timeout).await {
                    Ok(()) => {
                        info!(
                            environment_id = env_id,
                            "Environment woke up, waiting for route reload"
                        );

                        // The woken environment was excluded from the route table
                        // while sleeping, so we must wait for the in-process
                        // reload (driven by Job::ForceRouteReload in do_wake)
                        // before the request can resolve. wait_for_route_reload
                        // is lost-wakeup-safe, but we still re-resolve in a
                        // bounded loop afterwards so a route that lands a few
                        // milliseconds late (or a missed signal) still serves THIS
                        // first request instead of falling back to the console.
                        let reload_timeout = std::time::Duration::from_secs(10);
                        let reloaded = on_demand.wait_for_route_reload(reload_timeout).await;
                        if !reloaded {
                            warn!(
                                environment_id = env_id,
                                "Route reload not observed within timeout after wake; \
                                 re-resolving route directly"
                            );
                        }

                        // Bounded re-resolve loop: poll resolve_context until the
                        // just-woken host is routable, or we exhaust the budget.
                        // We only need to CONFIRM the route is live here — the
                        // canonical resolve below does the actual context setup,
                        // attack-mode checks, and activity recording.
                        let resolve_deadline =
                            std::time::Instant::now() + std::time::Duration::from_secs(5);
                        // A context for a host without a live upstream (its
                        // containers not running yet) is not routable: keep
                        // polling rather than failing the first request.
                        let mut routable = self
                            .project_context_resolver
                            .resolve_context(&ctx.host)
                            .await
                            .is_some_and(|context| !context.upstream_unavailable);
                        while !routable && std::time::Instant::now() < resolve_deadline {
                            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                            routable = self
                                .project_context_resolver
                                .resolve_context(&ctx.host)
                                .await
                                .is_some_and(|context| !context.upstream_unavailable);
                        }

                        if routable {
                            info!(
                                environment_id = env_id,
                                "Route resolved after wake, serving first request"
                            );
                            // Fall through to normal request handling (the
                            // canonical resolve below now succeeds).
                        } else {
                            // Containers are awake but the route still isn't
                            // resolvable. Do NOT fall through — that would route
                            // the app's own domain to the console and serve a
                            // confusing error. Return an explicit, retryable 503
                            // so the client retries instead.
                            error!(
                                environment_id = env_id,
                                host = %ctx.host,
                                "Environment woke but route did not become resolvable; \
                                 returning retryable wake_pending"
                            );
                            let mut response =
                                ResponseHeader::build(StatusCode::SERVICE_UNAVAILABLE, None)?;
                            response.insert_header("Retry-After", "2")?;
                            response.insert_header("Cache-Control", "no-store")?;
                            response.insert_header("X-Request-ID", &ctx.request_id)?;
                            response.insert_header("Content-Type", "application/json")?;

                            // No environment_id in the body — see the wake_throttled
                            // response above. Detail stays server-side in the log line.
                            let body_bytes = Bytes::from_static(
                                br#"{"status":"wake_pending","message":"Environment is starting, please retry"}"#,
                            );

                            session
                                .write_response_header(Box::new(response), false)
                                .await?;
                            session.write_response_body(Some(body_bytes), true).await?;

                            ctx.routing_status = "wake_pending".to_string();
                            return Ok(true);
                        }
                    }
                    Err(e) => {
                        error!(
                            environment_id = env_id,
                            error = %e,
                            "Failed to wake environment"
                        );

                        // Wake failed — return 503 with Retry-After
                        let mut response =
                            ResponseHeader::build(StatusCode::SERVICE_UNAVAILABLE, None)?;
                        response.insert_header("Retry-After", "5")?;
                        response.insert_header("Cache-Control", "no-store")?;
                        response.insert_header("X-Request-ID", &ctx.request_id)?;
                        response.insert_header("Content-Type", "application/json")?;

                        // Static body: do not interpolate the OnDemandError Display
                        // string (it can carry container/deployment context) or the
                        // environment_id into a response served to an unauthenticated
                        // client. The detailed error is logged server-side above.
                        let body_bytes = Bytes::from_static(
                            br#"{"status":"wake_failed","message":"Failed to start environment, please retry"}"#,
                        );

                        session
                            .write_response_header(Box::new(response), false)
                            .await?;
                        session.write_response_body(Some(body_bytes), true).await?;

                        ctx.routing_status = "wake_failed".to_string();
                        return Ok(true);
                    }
                }
            }
        }

        // Path routes (ADR 050): `frontend: /api -> backend` serves this
        // path from another service of the project. In-memory lookup only.
        //
        // The password form rendered for a path-routed target posts to the
        // reserved `/_temps/password-verify`, which never matches a path
        // route; `resolve_request_path_route` matches it by the routed path
        // the form carries, so the password is checked (and the cookie
        // issued) for the target environment the form was rendered for,
        // never the host's own. The Temps-login exchange callback is matched
        // the same way, by the `next` path its sign-in started from, so the
        // grant is traded (and the cookie scoped) for the routed target.
        // Client input only selects among the prefixes this host already
        // serves.
        if ctx.preview_route.is_none() {
            ctx.path_route = crate::handler::password_wall::resolve_request_path_route(
                &ctx.method,
                &ctx.path,
                ctx.query_string.as_deref(),
                |path| {
                    self.project_context_resolver
                        .match_path_route(&ctx.host, path)
                },
            );
        }

        // Path-route wake-on-demand: if the target service is sleeping
        // (scale-to-zero), wake it inline before resolving the route, using the
        // same mechanism as host-level wake-on-demand.
        if let Some(sleeping_env_id) = ctx
            .path_route
            .as_ref()
            .and_then(|r| (r.target.is_none()).then_some(r.sleeping_environment_id))
            .flatten()
        {
            if let Some(ref on_demand) = self.on_demand_manager {
                if let Some(sleeping_info) =
                    on_demand.get_sleeping_environment_by_id(sleeping_env_id)
                {
                    let wake_timeout = sleeping_info.wake_timeout_seconds;
                    info!(
                        environment_id = sleeping_env_id,
                        host = %ctx.host,
                        prefix = %ctx.path_route.as_ref().map(|r| r.prefix.as_str()).unwrap_or(""),
                        "Path-route target is sleeping, waking inline"
                    );
                    let _wake_slot = match on_demand.try_acquire_wake_slot() {
                        Some(permit) => permit,
                        None => {
                            warn!(
                                environment_id = sleeping_env_id,
                                host = %ctx.host,
                                "Wake path at capacity for path-route target; returning retryable 503"
                            );
                            let mut response =
                                ResponseHeader::build(StatusCode::SERVICE_UNAVAILABLE, None)?;
                            response.insert_header("Retry-After", "2")?;
                            response.insert_header("Cache-Control", "no-store")?;
                            response.insert_header("X-Request-ID", &ctx.request_id)?;
                            response.insert_header("Content-Type", "application/json")?;
                            let body_bytes = Bytes::from_static(
                                br#"{"status":"wake_pending","message":"Environment is starting, please retry"}"#,
                            );
                            session
                                .write_response_header(Box::new(response), false)
                                .await?;
                            session.write_response_body(Some(body_bytes), true).await?;
                            ctx.routing_status = "wake_throttled".to_string();
                            return Ok(true);
                        }
                    };
                    match on_demand
                        .wake_environment(sleeping_env_id, wake_timeout)
                        .await
                    {
                        Ok(()) => {
                            info!(
                                environment_id = sleeping_env_id,
                                "Path-route target woke up, waiting for route reload"
                            );
                            let reload_timeout = std::time::Duration::from_secs(10);
                            let reloaded = on_demand.wait_for_route_reload(reload_timeout).await;
                            if !reloaded {
                                warn!(
                                    environment_id = sleeping_env_id,
                                    "Route reload not observed within timeout after path-route wake"
                                );
                            }
                            // Re-resolve the path route after the target is awake.
                            let resolve_deadline =
                                std::time::Instant::now() + std::time::Duration::from_secs(5);
                            let rematch = |ctx: &ProxyContext| {
                                crate::handler::password_wall::resolve_request_path_route(
                                    &ctx.method,
                                    &ctx.path,
                                    ctx.query_string.as_deref(),
                                    |path| {
                                        self.project_context_resolver
                                            .match_path_route(&ctx.host, path)
                                    },
                                )
                            };
                            ctx.path_route = rematch(ctx);
                            while ctx.path_route.as_ref().is_none_or(|r| r.target.is_none())
                                && std::time::Instant::now() < resolve_deadline
                            {
                                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                                ctx.path_route = rematch(ctx);
                            }
                            if ctx.path_route.as_ref().is_none_or(|r| r.target.is_none()) {
                                error!(
                                    environment_id = sleeping_env_id,
                                    host = %ctx.host,
                                    "Path-route target woke but route did not resolve; returning retryable 503"
                                );
                                let mut response =
                                    ResponseHeader::build(StatusCode::SERVICE_UNAVAILABLE, None)?;
                                response.insert_header("Retry-After", "2")?;
                                response.insert_header("Cache-Control", "no-store")?;
                                response.insert_header("X-Request-ID", &ctx.request_id)?;
                                response.insert_header("Content-Type", "application/json")?;
                                let body_bytes = Bytes::from_static(
                                    br#"{"status":"wake_pending","message":"Environment is starting, please retry"}"#,
                                );
                                session
                                    .write_response_header(Box::new(response), false)
                                    .await?;
                                session.write_response_body(Some(body_bytes), true).await?;
                                ctx.routing_status = "wake_pending".to_string();
                                return Ok(true);
                            }
                            // Fall through: ctx.path_route now has a live target.
                        }
                        Err(e) => {
                            error!(
                                environment_id = sleeping_env_id,
                                error = %e,
                                "Failed to wake path-route target environment"
                            );
                            let mut response =
                                ResponseHeader::build(StatusCode::SERVICE_UNAVAILABLE, None)?;
                            response.insert_header("Retry-After", "5")?;
                            response.insert_header("Cache-Control", "no-store")?;
                            response.insert_header("X-Request-ID", &ctx.request_id)?;
                            response.insert_header("Content-Type", "application/json")?;
                            let body_bytes = Bytes::from_static(
                                br#"{"status":"wake_failed","message":"Failed to start environment, please retry"}"#,
                            );
                            session
                                .write_response_header(Box::new(response), false)
                                .await?;
                            session.write_response_body(Some(body_bytes), true).await?;
                            ctx.routing_status = "wake_failed".to_string();
                            return Ok(true);
                        }
                    }
                }
            }
        }

        if let Some(path_route) = ctx.path_route.as_ref() {
            if path_route.target.is_none() {
                // Never let the host's own service (or another environment)
                // answer in the target's place: that hides the problem
                // behind the frontend's index.html. The body carries only
                // the requested prefix; the service name goes to the log.
                warn!(
                    host = %ctx.host,
                    prefix = %path_route.prefix,
                    target_service = %path_route.target_service,
                    "path route target has no running deployment in this environment"
                );
                let body = Bytes::from(
                    serde_json::json!({
                        "status": "path_route_unavailable",
                        "message": format!(
                            "The service that serves {} is not deployed in this environment",
                            path_route.prefix
                        ),
                    })
                    .to_string(),
                );
                let mut response = ResponseHeader::build(StatusCode::BAD_GATEWAY, None)?;
                response.insert_header("Cache-Control", "no-store")?;
                response.insert_header("X-Request-ID", &ctx.request_id)?;
                response.insert_header("Content-Type", "application/json")?;
                response.insert_header("Content-Length", body.len().to_string())?;
                session
                    .write_response_header(Box::new(response), false)
                    .await?;
                session.write_response_body(Some(body), true).await?;
                ctx.routing_status = "path_route_unavailable".to_string();
                return Ok(true);
            }
        }

        // Resolve project context early to set routing status for all requests
        let path_target_context = ctx
            .path_route
            .as_ref()
            .and_then(|route| route.target.as_ref())
            .and_then(|target| {
                Some(ProjectContext {
                    project: target.project.clone()?,
                    environment: target.environment.clone()?,
                    deployment: target.deployment.clone()?,
                    // A target is only attached once it has a live backend.
                    upstream_unavailable: false,
                })
            });
        let project_context = match path_target_context {
            Some(context) => Some(context),
            None => {
                self.project_context_resolver
                    .resolve_context(&ctx.host)
                    .await
            }
        };

        if let Some(project_ctx) = &project_context {
            ctx.project = Some(project_ctx.project.clone());
            ctx.environment = Some(project_ctx.environment.clone());
            ctx.deployment = Some(project_ctx.deployment.clone());
            ctx.routing_status = "routed".to_string();

            // Record activity for on-demand idle tracking. A request that
            // will be answered 503 (no live upstream) is not activity: it must
            // not keep an environment with dead containers from idling to
            // sleep, after which the next request wakes it cleanly.
            if let Some(ref on_demand) = self.on_demand_manager {
                if !project_ctx.upstream_unavailable {
                    on_demand.record_activity(project_ctx.environment.id);
                }
            }

            // Per-project/environment concurrent-connection cap (issue #646): a slow
            // or malicious upstream must not be able to exhaust the proxy's own
            // connection budget now that request timeouts are opt-in (PR #642). Keyed
            // on environment id (the actual upstream-instance granularity); global
            // default and the project/environment DeploymentConfig override are
            // resolved the same way as the timeout settings above.
            // One TTL-cached settings read serves both the connection cap and
            // the rate limits below (an in-memory read, not a DB round-trip).
            let (connection_limits, instance_rate_limiting) = self
                .config_service
                .get_settings()
                .await
                .map(|settings| (settings.connection_limits, settings.rate_limiting))
                .unwrap_or_default();
            let project_config = project_ctx
                .project
                .deployment_config
                .clone()
                .unwrap_or_default();
            let effective_config = project_ctx
                .environment
                .get_effective_deployment_config(&project_config);
            let connection_limit = effective_config
                .max_concurrent_connections
                .map(|v| v.max(0) as u32)
                .unwrap_or(connection_limits.default_max_concurrent_connections);

            match self
                .connection_limiter
                .try_acquire(project_ctx.environment.id, connection_limit)
            {
                Some(permit) => ctx.connection_permit = Some(permit),
                None => {
                    warn!(
                        environment_id = project_ctx.environment.id,
                        project_id = project_ctx.project.id,
                        limit = connection_limit,
                        "Environment at concurrent-connection capacity; rejecting request"
                    );
                    let mut response =
                        ResponseHeader::build(StatusCode::SERVICE_UNAVAILABLE, None)?;
                    response.insert_header("Retry-After", "1")?;
                    response.insert_header("Cache-Control", "no-store")?;
                    response.insert_header("X-Request-ID", &ctx.request_id)?;
                    response.insert_header("Content-Type", "application/json")?;
                    // Generic body/message: this must not be distinguishable from the
                    // proxy's other 503 responses (e.g. no upstream route found), or
                    // an unauthenticated caller could use it as an oracle to confirm
                    // that a given hostname routes to a real project/environment.
                    let body_bytes = Bytes::from_static(
                        br#"{"status":"service_unavailable","message":"Service temporarily unavailable, please retry"}"#,
                    );
                    session
                        .write_response_header(Box::new(response), false)
                        .await?;
                    session.write_response_body(Some(body_bytes), true).await?;
                    ctx.routing_status = "connection_limit_exceeded".to_string();
                    return Ok(true);
                }
            }

            // Per-project/environment IP restriction. Synchronous,
            // lock-free — see temps_core::ProjectIpGate's contract. Denial
            // is a generic 403 with no detail: an unauthenticated caller
            // must not be able to distinguish "this project doesn't exist"
            // from "this project is restricted and you're not on the
            // allowlist" by response shape.
            //
            // `ctx.ip_address` can fail to resolve to a parseable `IpAddr`
            // (non-INET socket, missing/garbled forwarded-for value —
            // resolve_session_client_ip falls back to the literal
            // "unknown"). We must not silently skip enforcement in that
            // case: fail closed (deny) when this project/environment
            // actually has an active restriction policy, since an
            // unresolvable IP under an active policy is exactly what an
            // attacker (or a misconfigured proxy) would produce. When there
            // is no policy at all for this project/environment — the common
            // case — an unresolvable IP must NOT deny, matching today's
            // behavior; this feature is opt-in and most traffic has nothing
            // to do with it.
            let parsed_ip = ctx
                .ip_address
                .as_deref()
                .and_then(|s| s.parse::<std::net::IpAddr>().ok())
                .map(normalize_client_ip);
            let decision = evaluate_request_policy(
                self.request_policy_gate.as_ref(),
                session.req_header(),
                &ctx.host,
                project_ctx.project.id,
                project_ctx.environment.id,
                parsed_ip,
            );
            let ip_restricted = legacy_ip_gate_denies(
                decision,
                self.project_ip_gate.as_ref(),
                project_ctx.project.id,
                project_ctx.environment.id,
                parsed_ip,
            );
            if let temps_core::RequestPolicyDecision::Unavailable { reason } = decision {
                warn!(
                    project_id = project_ctx.project.id,
                    environment_id = project_ctx.environment.id,
                    reason,
                    "Project policy unavailable; denying request"
                );
                let mut response = ResponseHeader::build(StatusCode::SERVICE_UNAVAILABLE, None)?;
                response.insert_header("Cache-Control", "no-store")?;
                response.insert_header("X-Request-ID", &ctx.request_id)?;
                response.insert_header("Content-Type", "text/plain; charset=utf-8")?;
                session
                    .write_response_header(Box::new(response), false)
                    .await?;
                session
                    .write_response_body(Some(Bytes::from_static(b"Service unavailable\n")), true)
                    .await?;
                ctx.routing_status = "request_policy_unavailable".to_string();
                return Ok(true);
            }
            if let temps_core::RequestPolicyDecision::Deny {
                reason,
                rule_id,
                revision,
            } = decision
            {
                warn!(
                    project_id = project_ctx.project.id,
                    environment_id = project_ctx.environment.id,
                    reason,
                    rule_id,
                    revision,
                    "Request denied by project policy"
                );
                let mut response = ResponseHeader::build(StatusCode::FORBIDDEN, None)?;
                response.insert_header("Cache-Control", "no-store")?;
                response.insert_header("X-Request-ID", &ctx.request_id)?;
                response.insert_header("Content-Type", "text/plain; charset=utf-8")?;
                session
                    .write_response_header(Box::new(response), false)
                    .await?;
                session
                    .write_response_body(Some(Bytes::from_static(b"Forbidden\n")), true)
                    .await?;
                ctx.routing_status = "request_policy_denied".to_string();
                return Ok(true);
            }

            let password_protection = project_ctx
                .environment
                .deployment_config
                .as_ref()
                .and_then(|dc| dc.security.as_ref())
                .and_then(|s| s.password_protection.as_ref())
                .filter(|pp| pp.enabled);
            // The password form's POST: with a wall, the wall always handles
            // it (it never reaches the app), so a rule must not stop visitors
            // from unlocking. Without one, the path is the app's.
            let password_verify_post = password_protection.is_some()
                && ctx.path == crate::handler::password_wall::PASSWORD_VERIFY_PATH
                && ctx.method == "POST";

            // Project edge rules (ADR-089): after the operator's IP gate,
            // before rate limits, Shield and the password wall, none of which
            // an `allow` rule skips. ACME, Temps' own challenge endpoints and
            // the password form are never subject to them.
            if !password_verify_post
                && self
                    .enforce_edge_rules(
                        session,
                        ctx,
                        project_ctx.project.id,
                        project_ctx.environment.id,
                        parsed_ip,
                    )
                    .await?
            {
                return Ok(true);
            }

            // The access decision (ADR-055 §11): the legacy chain, or the
            // environment's compiled ingress endpoint, as its
            // `ingress_mode` says. Either way it is one `AccessOutcome`,
            // answered below by the same responses.
            let access_outcome = self
                .decide_environment_access(
                    project_ctx,
                    AccessRequest {
                        request: session.req_header(),
                        host: &ctx.host,
                        path: &ctx.path,
                        tls_fingerprint: ctx.tls_fingerprint.as_deref(),
                        policy_decision: decision,
                        ip_restricted,
                        client_ip: parsed_ip,
                        rate_limit: crate::rate_limiter::resolve_policy(
                            &instance_rate_limiting,
                            effective_config.security.as_ref(),
                            project_ctx.environment.id,
                        ),
                        password: password_protection.map(|pp| crate::ingress::PasswordFacts {
                            verify_post: ctx.path
                                == crate::handler::password_wall::PASSWORD_VERIFY_PATH
                                && ctx.method == "POST",
                            cookie_valid: find_request_cookie(
                                session.req_header(),
                                crate::handler::password_wall::PASSWORD_COOKIE_NAME,
                                |value| {
                                    crate::handler::password_wall::validate_cookie(
                                        value,
                                        project_ctx.environment.id,
                                        &pp.password_hash,
                                    )
                                    .then_some(true)
                                },
                            )
                            .unwrap_or(false),
                        }),
                    },
                )
                .await;

            match access_outcome {
                crate::ingress::AccessOutcome::Allow
                | crate::ingress::AccessOutcome::PasswordWall => {}
                crate::ingress::AccessOutcome::IpForbidden
                | crate::ingress::AccessOutcome::Forbidden => {
                    // Counted for every denial; logged through a
                    // process-wide limiter so a scan against a locked
                    // project cannot flood the log (the counter keeps
                    // the true rate).
                    self.proxy_metrics.record_ip_access_denied();
                    let now_secs = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    if let Some(suppressed) = IP_DENY_LOG_LIMITER.admit(now_secs) {
                        warn!(
                            environment_id = project_ctx.environment.id,
                            project_id = project_ctx.project.id,
                            ip = %parsed_ip.map(|ip| ip.to_string()).unwrap_or_else(|| "unresolved".to_string()),
                            outcome = access_outcome.label(),
                            suppressed_since_last_log = suppressed,
                            "Request denied by project IP restriction"
                        );
                    }
                    // A generic 403 with no detail: an unauthenticated
                    // caller must not be able to tell "this project is
                    // restricted" from "this project does not exist".
                    let mut response = ResponseHeader::build(StatusCode::FORBIDDEN, None)?;
                    response.insert_header("Cache-Control", "no-store")?;
                    response.insert_header("X-Request-ID", &ctx.request_id)?;
                    response.insert_header("Content-Type", "text/plain; charset=utf-8")?;
                    session
                        .write_response_header(Box::new(response), false)
                        .await?;
                    session
                        .write_response_body(Some(Bytes::from_static(b"Forbidden\n")), true)
                        .await?;
                    ctx.routing_status =
                        if access_outcome == crate::ingress::AccessOutcome::IpForbidden {
                            "project_ip_restricted"
                        } else {
                            "ingress_forbidden"
                        }
                        .to_string();
                    return Ok(true);
                }
                crate::ingress::AccessOutcome::Blacklisted => {
                    self.proxy_metrics.record_rate_limited();
                    warn!(
                        project_id = project_ctx.project.id,
                        environment_id = project_ctx.environment.id,
                        ip = %parsed_ip.map(|ip| ip.to_string()).unwrap_or_else(|| "unresolved".to_string()),
                        "Request denied by rate-limit blacklist"
                    );
                    // Same generic 403 as the IP restriction above.
                    let mut response = ResponseHeader::build(StatusCode::FORBIDDEN, None)?;
                    response.insert_header("Cache-Control", "no-store")?;
                    response.insert_header("X-Request-ID", &ctx.request_id)?;
                    response.insert_header("Content-Type", "text/plain; charset=utf-8")?;
                    session
                        .write_response_header(Box::new(response), false)
                        .await?;
                    session
                        .write_response_body(Some(Bytes::from_static(b"Forbidden\n")), true)
                        .await?;
                    ctx.routing_status = "rate_limit_blacklisted".to_string();
                    return Ok(true);
                }
                crate::ingress::AccessOutcome::TooManyRequests {
                    window,
                    retry_after_secs,
                } => {
                    self.proxy_metrics.record_rate_limited();
                    debug!(
                        project_id = project_ctx.project.id,
                        environment_id = project_ctx.environment.id,
                        ip = %parsed_ip.map(|ip| ip.to_string()).unwrap_or_else(|| "unresolved".to_string()),
                        window = window.as_str(),
                        "Request rejected by rate limit"
                    );
                    let mut response = ResponseHeader::build(StatusCode::TOO_MANY_REQUESTS, None)?;
                    response.insert_header("Retry-After", retry_after_secs.to_string())?;
                    response.insert_header("Cache-Control", "no-store")?;
                    response.insert_header("X-Request-ID", &ctx.request_id)?;
                    response.insert_header("Content-Type", "text/plain; charset=utf-8")?;
                    session
                        .write_response_header(Box::new(response), false)
                        .await?;
                    session
                        .write_response_body(Some(Bytes::from_static(b"Too Many Requests\n")), true)
                        .await?;
                    ctx.routing_status = "rate_limited".to_string();
                    return Ok(true);
                }
                crate::ingress::AccessOutcome::Unavailable => {
                    // Logged (rate-limited, with the reason) where it was decided.
                    let mut response =
                        ResponseHeader::build(StatusCode::SERVICE_UNAVAILABLE, None)?;
                    response.insert_header("Cache-Control", "no-store")?;
                    response.insert_header("X-Request-ID", &ctx.request_id)?;
                    response.insert_header("Content-Type", "text/plain; charset=utf-8")?;
                    session
                        .write_response_header(Box::new(response), false)
                        .await?;
                    session
                        .write_response_body(
                            Some(Bytes::from_static(
                                b"Service unavailable: this environment's access policy is not available\n",
                            )),
                            true,
                        )
                        .await?;
                    ctx.routing_status = "ingress_endpoint_unavailable".to_string();
                    return Ok(true);
                }
                crate::ingress::AccessOutcome::LoginRequired
                    if ctx.path == crate::ingress::GRANT_EXCHANGE_PATH =>
                {
                    let secure = self.is_https_request(session);
                    // A path-routed target (resolved from the exchange's
                    // `next`) gets a cookie scoped to its prefix.
                    let exchange = self.exchange_ingress_grant(
                        project_ctx.environment.id,
                        &ctx.host,
                        ctx.query_string.as_deref(),
                        secure,
                        ctx.path_route.as_ref().map(|route| route.prefix.as_str()),
                    );
                    // The query holds the grant: keep it out of the proxy
                    // log, which outlives the grant and is readable by
                    // project members.
                    ctx.query_string = None;
                    let mut response = match exchange {
                        Ok(exchange) => {
                            info!(
                                project_id = project_ctx.project.id,
                                environment_id = project_ctx.environment.id,
                                grant_id = exchange.grant_id,
                                user_id = exchange.user_id,
                                "Temps login grant exchanged for an ingress session"
                            );
                            let mut response = ResponseHeader::build(StatusCode::SEE_OTHER, None)?;
                            response.insert_header("Location", exchange.location)?;
                            response.insert_header("Set-Cookie", exchange.set_cookie)?;
                            response
                        }
                        Err(error) => {
                            warn!(
                                project_id = project_ctx.project.id,
                                environment_id = project_ctx.environment.id,
                                error = %error,
                                "Refused a Temps login grant exchange"
                            );
                            ResponseHeader::build(StatusCode::FORBIDDEN, None)?
                        }
                    };
                    let refused = response.status != StatusCode::SEE_OTHER;
                    response.insert_header("Cache-Control", "no-store")?;
                    response.insert_header("Referrer-Policy", "no-referrer")?;
                    response.insert_header("X-Request-ID", &ctx.request_id)?;
                    response.insert_header("Content-Type", "text/plain; charset=utf-8")?;
                    session
                        .write_response_header(Box::new(response), false)
                        .await?;
                    let body: &'static [u8] = if refused {
                        b"This sign-in link is invalid or has expired. Reload the page to sign in again.\n"
                    } else {
                        b"Signed in.\n"
                    };
                    session
                        .write_response_body(Some(Bytes::from_static(body)), true)
                        .await?;
                    ctx.routing_status = if refused {
                        "ingress_grant_refused"
                    } else {
                        "ingress_grant_exchanged"
                    }
                    .to_string();
                    return Ok(true);
                }
                // An edge rule challenge runs before this decision, so its
                // solver and verification must work before sign-in; Shield
                // answers them below.
                crate::ingress::AccessOutcome::LoginRequired
                    if crate::ingress::answered_before_login(&ctx.path) => {}
                crate::ingress::AccessOutcome::LoginRequired => {
                    let external_url = self
                        .config_service
                        .get_settings()
                        .await
                        .ok()
                        .and_then(|settings| settings.external_url);
                    let next = match ctx.query_string.as_deref() {
                        Some(qs) if !qs.is_empty() => format!("{}?{}", ctx.path, qs),
                        _ => ctx.path.clone(),
                    };
                    let scheme = if self.is_https_request(session) {
                        crate::ingress::LoginScheme::Https
                    } else {
                        crate::ingress::LoginScheme::Http
                    };
                    let now_secs = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    let port = self
                        .request_authority(session)
                        .ok()
                        .and_then(|authority| authority.port);
                    let location = external_url.as_deref().and_then(|base| {
                        crate::ingress::login_redirect_url(
                            base,
                            self.crypto.ingress_grant_master_key(),
                            temps_workload_ingress::ProductRef {
                                kind: temps_workload_ingress::ProductKind::Environment,
                                id: i64::from(project_ctx.environment.id),
                            },
                            crate::ingress::LoginSite {
                                scheme,
                                host: &ctx.host,
                                port,
                            },
                            &next,
                            now_secs,
                        )
                    });
                    let mut response = match location {
                        Some(location) => {
                            let mut response = ResponseHeader::build(StatusCode::SEE_OTHER, None)?;
                            response.insert_header("Location", location)?;
                            response
                        }
                        None => {
                            warn!(
                                project_id = project_ctx.project.id,
                                environment_id = project_ctx.environment.id,
                                "Environment requires a Temps login but the instance has no valid external URL to send the visitor to"
                            );
                            ResponseHeader::build(StatusCode::FORBIDDEN, None)?
                        }
                    };
                    response.insert_header("Cache-Control", "no-store")?;
                    response.insert_header("Referrer-Policy", "no-referrer")?;
                    response.insert_header("X-Request-ID", &ctx.request_id)?;
                    response.insert_header("Content-Type", "text/plain; charset=utf-8")?;
                    session
                        .write_response_header(Box::new(response), false)
                        .await?;
                    session
                        .write_response_body(
                            Some(Bytes::from_static(b"Sign in to Temps to open this page.\n")),
                            true,
                        )
                        .await?;
                    ctx.routing_status = "ingress_login_required".to_string();
                    return Ok(true);
                }
                // Bot challenges are Shield's (ADR 066), which runs right
                // after this decision; the access decision defers to it.
                crate::ingress::AccessOutcome::HttpsRequired
                | crate::ingress::AccessOutcome::CaptchaChallenge => {}
            }

            // ADR 066 (I4): Shield, after the operator's IP gate and the
            // rate limits, so an operator deny rule or a spent rate budget
            // answers first and is not a Shield decision (D1). The verify
            // POST and the solver the page loads are answered here, whichever
            // environment this host or path resolved to: the clearance is for
            // the environment in the signed token, and a page served just
            // before Shield was turned off can still finish. Neither they nor
            // ACME HTTP-01 are ever challenged (D7).
            if ctx.path == crate::shield::VERIFY_PATH {
                return self
                    .answer_challenge_verify(
                        session,
                        ctx,
                        &project_ctx.project,
                        &project_ctx.environment,
                        parsed_ip,
                    )
                    .await;
            }
            if self.serve_solver_asset(session, ctx).await? {
                return Ok(true);
            }
            // Only a challenge stored for this host skips Shield; any other
            // path under the ACME prefix is decided like the rest.
            if self.answer_stored_acme_challenge(session, ctx).await? {
                return Ok(true);
            }
            if self
                .enforce_shield(
                    session,
                    ctx,
                    &project_ctx.project,
                    &project_ctx.environment,
                    parsed_ip,
                )
                .await?
            {
                return Ok(true);
            }

            // Password wall: the outcome says the wall decides this request
            // (no valid cookie, or the wall's own verify POST).
            if access_outcome == crate::ingress::AccessOutcome::PasswordWall
                && password_protection.is_none()
            {
                // The compiled endpoint asked for a wall this environment no
                // longer has (a reload in flight): refuse rather than guess.
                let mut response = ResponseHeader::build(StatusCode::SERVICE_UNAVAILABLE, None)?;
                response.insert_header("Retry-After", "1")?;
                response.insert_header("Cache-Control", "no-store")?;
                response.insert_header("X-Request-ID", &ctx.request_id)?;
                response.insert_header("Content-Type", "text/plain; charset=utf-8")?;
                session
                    .write_response_header(Box::new(response), false)
                    .await?;
                session
                    .write_response_body(Some(Bytes::from_static(b"Service unavailable\n")), true)
                    .await?;
                ctx.routing_status = "ingress_endpoint_unavailable".to_string();
                return Ok(true);
            }
            if let Some(pp) = password_protection
                .filter(|_| access_outcome == crate::ingress::AccessOutcome::PasswordWall)
            {
                let password_hash = pp.password_hash.clone();
                let env_id = project_ctx.environment.id;
                let project_name = &project_ctx.project.name;
                let environment_name = &project_ctx.environment.name;
                // Check if this is the password verify POST endpoint
                if ctx.path == crate::handler::password_wall::PASSWORD_VERIFY_PATH
                    && ctx.method == "POST"
                {
                    // Read the POST body to get the password
                    let body = session.read_request_body().await.map_err(|e| {
                        error!("Failed to read password verify body: {}", e);
                        e
                    })?;

                    let body_str = body
                        .as_ref()
                        .map(|b| String::from_utf8_lossy(b).to_string())
                        .unwrap_or_default();

                    // Parse form data (application/x-www-form-urlencoded)
                    let params: Vec<(String, String)> =
                        url::form_urlencoded::parse(body_str.as_bytes())
                            .into_owned()
                            .collect();

                    let password = params
                        .iter()
                        .find(|(k, _)| k == "password")
                        .map(|(_, v)| v.as_str())
                        .unwrap_or("");

                    // The destination is client input: reduce it to a
                    // same-origin path before it becomes a Location header.
                    let redirect = crate::handler::password_wall::sanitize_redirect_path(
                        params
                            .iter()
                            .find(|(k, _)| k == "redirect")
                            .map(|(_, v)| v.as_str())
                            .unwrap_or("/"),
                    );

                    // Guesses are limited per (client IP, environment) with
                    // the same sliding window as the sandbox preview login,
                    // so the wall cannot be brute-forced. The attempt is
                    // taken atomically before the password is checked, so a
                    // concurrent burst cannot outrun the count. An unparsable
                    // client address shares one bucket rather than escaping
                    // the limit.
                    let client_ip = ctx
                        .ip_address
                        .as_deref()
                        .and_then(|s| s.parse::<std::net::IpAddr>().ok())
                        .unwrap_or_else(|| std::net::IpAddr::from([127, 0, 0, 1]));
                    if !self
                        .preview_auth_limiter
                        .try_admit_password_wall(client_ip, env_id)
                    {
                        warn!(
                            environment_id = env_id,
                            client_ip = %client_ip,
                            "password-wall: verify POST rate limited"
                        );
                        let mut resp = ResponseHeader::build(StatusCode::TOO_MANY_REQUESTS, None)?;
                        resp.insert_header("Retry-After", "60")?;
                        resp.insert_header("Cache-Control", "no-store")?;
                        resp.insert_header("X-Request-ID", &ctx.request_id)?;
                        resp.insert_header("Content-Type", "text/plain; charset=utf-8")?;
                        resp.insert_header("Referrer-Policy", "no-referrer")?;
                        resp.insert_header("X-Frame-Options", "DENY")?;
                        session.write_response_header(Box::new(resp), false).await?;
                        session
                            .write_response_body(
                                Some(Bytes::from_static(
                                    b"Too many failed attempts. Try again in a minute.\n",
                                )),
                                true,
                            )
                            .await?;
                        ctx.routing_status = "password_rate_limited".to_string();
                        return Ok(true);
                    }

                    if crate::handler::password_wall::verify_password(password, &password_hash) {
                        // Password correct — set cookie and redirect
                        self.preview_auth_limiter
                            .clear_password_wall(client_ip, env_id);
                        let host = ctx.host.clone();
                        let set_cookie =
                            crate::handler::password_wall::build_set_cookie_header_for_path(
                                env_id,
                                &password_hash,
                                &host,
                                // A path-routed target is unlocked under
                                // its prefix only, so its cookie coexists
                                // with the host's own `Path=/` one.
                                ctx.path_route
                                    .as_ref()
                                    .map_or("/", |route| route.prefix.as_str()),
                            );

                        let mut resp = ResponseHeader::build(303, None)?;
                        resp.insert_header("Location", redirect)?;
                        resp.insert_header("Set-Cookie", &set_cookie)?;
                        resp.insert_header("Cache-Control", "no-store")?;
                        resp.insert_header("Referrer-Policy", "no-referrer")?;
                        resp.insert_header("X-Request-ID", &ctx.request_id)?;

                        session.write_response_header(Box::new(resp), true).await?;
                        ctx.routing_status = "password_verified".to_string();
                        return Ok(true);
                    } else {
                        // Wrong password — the attempt is already counted;
                        // show the form again with an error
                        let html =
                            crate::handler::password_wall::generate_password_form_html_for_route(
                                redirect,
                                true,
                                project_name,
                                environment_name,
                                // Behind a path route the form posts back
                                // the routed path, so the next attempt
                                // resolves to this environment again.
                                crate::handler::password_wall::routed_form_path(
                                    ctx.path_route.is_some(),
                                    &ctx.path,
                                    ctx.query_string.as_deref(),
                                )
                                .as_deref(),
                            );
                        let html_bytes = Bytes::from(html);

                        let mut resp = ResponseHeader::build(StatusCode::OK, None)?;
                        resp.insert_header("Content-Type", "text/html; charset=utf-8")?;
                        resp.insert_header("Cache-Control", "no-store")?;
                        resp.insert_header("X-Request-ID", &ctx.request_id)?;
                        // A credential prompt must not be framed or leak its URL.
                        resp.insert_header("Referrer-Policy", "no-referrer")?;
                        resp.insert_header("X-Frame-Options", "DENY")?;

                        session.write_response_header(Box::new(resp), false).await?;
                        session.write_response_body(Some(html_bytes), true).await?;
                        ctx.routing_status = "password_wrong".to_string();
                        return Ok(true);
                    }
                }

                // Check for valid password cookie. Several cookies may carry
                // this name: the host's own environment (`Path=/`) and a
                // path-routed target's (`Path=<prefix>`) coexist, and the
                // browser sends both on a routed path. Each is bound to its
                // environment id and password hash, so any one that
                // validates for THIS environment unlocks it.
                let has_valid_cookie = find_request_cookie(
                    session.req_header(),
                    crate::handler::password_wall::PASSWORD_COOKIE_NAME,
                    |value| {
                        crate::handler::password_wall::validate_cookie(
                            value,
                            env_id,
                            &password_hash,
                        )
                        .then_some(true)
                    },
                )
                .unwrap_or(false);

                if !has_valid_cookie {
                    // No valid cookie — show password form
                    let current_path = if let Some(ref qs) = ctx.query_string {
                        if qs.is_empty() {
                            ctx.path.clone()
                        } else {
                            format!("{}?{}", ctx.path, qs)
                        }
                    } else {
                        ctx.path.clone()
                    };

                    let html = crate::handler::password_wall::generate_password_form_html_for_route(
                        &current_path,
                        false,
                        project_name,
                        environment_name,
                        // Behind a path route the form posts back the routed
                        // path, so the verify request resolves to this
                        // (target) environment, not the host's own.
                        crate::handler::password_wall::routed_form_path(
                            ctx.path_route.is_some(),
                            &ctx.path,
                            ctx.query_string.as_deref(),
                        )
                        .as_deref(),
                    );
                    let html_bytes = Bytes::from(html);

                    let mut resp = ResponseHeader::build(StatusCode::OK, None)?;
                    resp.insert_header("Content-Type", "text/html; charset=utf-8")?;
                    resp.insert_header("Cache-Control", "no-store")?;
                    resp.insert_header("X-Request-ID", &ctx.request_id)?;
                    // A credential prompt must not be framed or leak its URL.
                    resp.insert_header("Referrer-Policy", "no-referrer")?;
                    resp.insert_header("X-Frame-Options", "DENY")?;

                    session.write_response_header(Box::new(resp), false).await?;
                    session.write_response_body(Some(html_bytes), true).await?;
                    ctx.routing_status = "password_wall".to_string();
                    return Ok(true);
                }
            }
        } else {
            ctx.routing_status = "no_project".to_string();
        }

        // The challenge solver, for hosts without a project (project hosts
        // were answered with Shield above).
        if self.serve_solver_asset(session, ctx).await? {
            return Ok(true);
        }

        // Handle ACME HTTP-01 challenges BEFORE redirects
        // This ensures domains configured as redirects can still complete certificate provisioning
        if let Some(key_authorization) = self
            .handle_acme_http_challenge(&ctx.host, &ctx.path)
            .await?
        {
            debug!(
                "Serving ACME HTTP-01 challenge response for {}{} (request_id={}) - before redirect check",
                ctx.host, ctx.path, ctx.request_id
            );

            let key_auth_bytes = Bytes::from(key_authorization.clone());
            let content_length = key_auth_bytes.len();

            let mut resp = ResponseHeader::build(200, None)?;
            resp.insert_header("Content-Type", "text/plain")?;
            resp.insert_header("Cache-Control", "no-cache")?;
            resp.insert_header("X-Request-ID", &ctx.request_id)?;
            resp.insert_header("Content-Length", content_length.to_string())?;
            resp.insert_header("Connection", "close")?;

            session.write_response_header(Box::new(resp), false).await?;
            session
                .write_response_body(Some(key_auth_bytes), true)
                .await?;

            info!(
                "ACME challenge completed (redirect domain): {} {} - 200 OK - {}ms",
                ctx.method,
                ctx.path,
                ctx.start_time.elapsed().as_millis()
            );

            ctx.routing_status = "acme_challenge".to_string();
            return Ok(true);
        }

        // On-demand HTTP-01 TLS UX (ADR-018 §5). Only engaged when the manager
        // is wired (on-demand TLS enabled) — otherwise zero overhead, no extra
        // settings fetch. MUST come after ACME challenge handling so a challenge
        // can always complete, and before the HTTPS redirect so a host whose
        // cert is still provisioning gets the 503 instead of a redirect to a
        // non-existent cert.
        // Gate on the cheap, in-memory checks FIRST so the common case (HTTPS
        // traffic, and HTTP hosts with no on-demand cert state) costs nothing.
        // `get_settings()` is TTL-cached (no Postgres round-trip), but the lazy
        // fetch is still kept inside `handle_on_demand_http` so it only runs for
        // the rare ephemeral `redirect_to_env` branch rather than for every
        // plain-HTTP request.
        if self.on_demand_cert_manager.is_some()
            && !self.is_tls_connection(session)
            && self.handle_on_demand_http(session, ctx).await?
        {
            return Ok(true);
        }

        // HTTP to HTTPS redirect for non-TLS connections.
        // This MUST come after ACME challenge handling to allow Let's Encrypt
        // HTTP-01 validation. `should_redirect_to_https` additionally exempts the
        // whole `/.well-known/acme-challenge/` prefix, so a validation request
        // that did NOT match a stored token (renewal in flight, token written to
        // a sibling domain row, wildcard parent) still falls through to normal
        // routing instead of being 301'd to a certificate that has expired or
        // does not exist yet.
        //
        // By default the redirect is per-domain: we only redirect when the
        // requesting host actually has an active TLS certificate in the database
        // (exact match or wildcard parent). This means HTTP-only installs
        // (sslip.io quick/local modes, no cert provisioned) never get redirected,
        // while hosts that have gone through SSL provisioning get automatic HTTPS
        // enforcement.
        //
        // The environment resolved above can override that default in either
        // direction (`force_https`): `Some(true)` for sites whose TLS is
        // terminated by an upstream CDN — no local cert exists, so the default
        // heuristic would leave plain HTTP serving a full 200 alongside HTTPS —
        // and `Some(false)` for environments that must stay reachable over HTTP.
        // Reading it off `ctx.environment` costs nothing extra: the environment
        // was already resolved and cloned into the context earlier in this
        // filter, so there is no additional lookup on the hot path.
        //
        // `disable_https_redirect` is a global escape hatch (set by the service
        // unit in local/testing mode) that bypasses the check entirely and
        // outranks the per-environment override.
        // WS3: cert-host check is now a lock-free ArcSwap snapshot read; the
        // background `CertHostCache::run_refresh_loop` keeps it current (±30 s).
        let env_force_https = ctx.environment.as_ref().and_then(|env| env.force_https);
        // An unresolved Host falls through to the console upstream. The console
        // has its own operator-set override rather than inheriting a project's,
        // and it is consulted only when no environment matched — `get_settings`
        // is TTL-cached, so this costs nothing on the project hot path.
        //
        // On a settings read failure this yields `None` (inherit the per-host
        // certificate heuristic), not `Some(true)`: guessing "redirect" here
        // would turn a transient database blip into a console that 301s to a
        // certificate it may not have.
        let console_force_https = if ctx.environment.is_none() {
            match self.config_service.get_settings().await {
                Ok(settings) => {
                    let request_host = ctx
                        .host
                        .split(':')
                        .next()
                        .unwrap_or(&ctx.host)
                        .trim_end_matches('.')
                        .to_ascii_lowercase();
                    if settings.console_hostname().as_deref() == Some(request_host.as_str()) {
                        settings.console_force_https
                    } else {
                        None
                    }
                }
                Err(error) => {
                    warn!(
                        error = %error,
                        "Failed to read app settings; falling back to the per-host certificate heuristic for HTTPS policy"
                    );
                    None
                }
            }
        } else {
            None
        };
        // Exactly one of these can be set: `console_force_https` is only
        // computed when no environment resolved.
        let force_https = env_force_https.or(console_force_https);
        let production_https = if should_apply_production_https_default(
            self.disable_https_redirect,
            self.is_tls_connection(session),
            &ctx.path,
            force_https,
            ctx.environment.is_some(),
        ) {
            match self.config_service.get_url_scheme().await {
                Ok(scheme) => scheme != "http",
                Err(error) => {
                    warn!(
                        error = %error,
                        "Failed to read external URL scheme; enforcing HTTPS"
                    );
                    true
                }
            }
        } else {
            false
        };
        let needs_redirect = should_redirect_to_https(
            self.disable_https_redirect,
            self.is_tls_connection(session),
            &ctx.path,
            force_https,
            // Lock-free ArcSwap snapshot read, and only reached when the
            // environment has no explicit override.
            || {
                inherited_https_policy(
                    production_https,
                    self.cert_host_cache.has_cert_for_host(&ctx.host),
                )
            },
        );
        if needs_redirect {
            // Build the HTTPS redirect URL preserving path and query string
            let redirect_url = if let Some(query) = &ctx.query_string {
                format!(
                    "https://{}{}{}",
                    ctx.host,
                    ctx.path,
                    if query.is_empty() {
                        String::new()
                    } else {
                        format!("?{}", query)
                    }
                )
            } else {
                format!("https://{}{}", ctx.host, ctx.path)
            };

            debug!(
                request_id = %ctx.request_id,
                host = %ctx.host,
                path = %ctx.path,
                redirect_url = %redirect_url,
                "Redirecting HTTP to HTTPS"
            );

            // Use 301 Permanent Redirect for HTTP→HTTPS
            let resp = https_redirect_response(&redirect_url, &ctx.request_id)?;
            // Managed monitors use this marker to distinguish the proxy's
            // pre-upstream protocol upgrade from an application's own 3xx.
            // It carries no trust decision by itself: monitors still require
            // a same-host HTTPS target and pin the follow-up to the configured
            // local TLS listener.
            ctx.routing_status = "http_to_https_redirect".to_string();

            session.write_response_header(Box::new(resp), true).await?;
            return Ok(true);
        }

        // Check if this host should redirect
        if let Some((redirect_target, status_code)) = self
            .project_context_resolver
            .get_redirect_info(&ctx.host)
            .await
        {
            let redirect_url =
                domain_redirect_location(&redirect_target, &ctx.path, ctx.query_string.as_deref());
            debug!(
                request_id = %ctx.request_id,
                host = %ctx.host,
                redirect_url = %redirect_url,
                status_code = status_code,
                "Redirecting request"
            );

            // Build redirect response
            let mut resp = ResponseHeader::build(status_code, None)?;
            resp.insert_header("Location", &redirect_url)?;
            resp.insert_header("Content-Length", "0")?;
            resp.insert_header("X-Temps-Proxy-Probe-Capable", "1")?;

            // Add CORS headers for redirect responses
            resp.insert_header("Access-Control-Allow-Origin", "*")?;

            // Update context for logging
            ctx.routing_status = "redirected".to_string();

            session.write_response_header(Box::new(resp), true).await?;
            return Ok(true); // Skip proxying
        }

        // RFC 7239 Forwarded, X-Real-IP, and CF-Connecting-IP are all
        // client-controlled at this trust boundary (any direct client can
        // set them, bypassing Bunny/Cloudflare entirely). We emit a
        // complete trusted X-Forwarded-* set below from the already-
        // resolved `ctx.ip_address`, so do not let a tenant app read a raw,
        // possibly-spoofed client-supplied header instead.
        strip_untrusted_client_ip_headers(
            session.req_header_mut(),
            self.loopback_ja4_header.load().as_deref(),
        );

        // Browsers split cookies across several fields over HTTP/2; the
        // upstream must receive them as one `Cookie` header or it keeps only
        // the first (see `coalesce_cookie_headers`).
        coalesce_cookie_headers(session.req_header_mut());

        // Capture request headers
        let request_headers: HashMap<String, String> = session
            .req_header()
            .headers
            .iter()
            .filter_map(|(k, v)| v.to_str().ok().map(|val| (k.to_string(), val.to_string())))
            .collect();
        ctx.request_headers = Some(request_headers);

        debug!(
            request_id = %ctx.request_id,
            method = %ctx.method,
            host = %ctx.host,
            path = %ctx.path,
            user_agent = %ctx.user_agent,
            "Incoming request"
        );

        // Store encrypted cookie values for later processing
        // Use project-scoped cookie names if project context is available
        let project_id = ctx.project.as_ref().map(|p| p.id);
        let visitor_cookie_name = get_visitor_cookie_name(project_id);
        let session_cookie_name = get_session_cookie_name(project_id);

        ctx.request_visitor_cookie =
            find_request_cookie(session.req_header(), &visitor_cookie_name, |value| {
                Some(value.to_string())
            });

        ctx.request_session_cookie =
            find_request_cookie(session.req_header(), &session_cookie_name, |value| {
                Some(value.to_string())
            });

        // Get IP from the connection
        // Add X-Forwarded-For header with client IP (already extracted in request_filter)
        if let Some(ref ip) = ctx.ip_address {
            session
                .req_header_mut()
                .insert_header("X-Forwarded-For", ip.as_str())?;
        }

        // Overwrite the complete public authority forwarded upstream. Apps
        // such as Keycloak trust this set when constructing absolute URLs.
        // Forwarding only the scheme loses non-default ports and produces
        // redirects to port 80/443 instead of the Temps proxy.
        let is_https = self.is_https_request(session);
        let proto = if is_https { "https" } else { "http" };
        let public_authority = self.request_authority(session)?;
        let forwarded_port = public_authority
            .port
            .unwrap_or(if is_https { 443 } else { 80 });
        // The same parsed authority controls routing and reaches the upstream.
        // Never pass the raw client Host after making a routing decision.
        session
            .req_header_mut()
            .insert_header("Host", public_authority.forwarded_host.clone())?;
        session
            .req_header_mut()
            .insert_header("X-Forwarded-Proto", proto)?;
        session
            .req_header_mut()
            .insert_header("X-Forwarded-Host", public_authority.forwarded_host)?;
        session
            .req_header_mut()
            .insert_header("X-Forwarded-Port", forwarded_port.to_string())?;

        ctx.referrer = session
            .req_header()
            .headers
            .get("referer")
            .map(|h| h.to_str().unwrap_or_default().to_string());

        // Handle ACME HTTP-01 challenges
        if let Some(key_authorization) = self
            .handle_acme_http_challenge(&ctx.host, &ctx.path)
            .await?
        {
            debug!(
                "Serving ACME HTTP-01 challenge response for {}{} (request_id={})",
                ctx.host, ctx.path, ctx.request_id
            );

            let key_auth_bytes = Bytes::from(key_authorization.clone());
            let content_length = key_auth_bytes.len();

            let mut resp = ResponseHeader::build(200, None)?;
            resp.insert_header("Content-Type", "text/plain")?;
            resp.insert_header("Cache-Control", "no-cache")?;
            resp.insert_header("X-Request-ID", &ctx.request_id)?;
            resp.insert_header("Content-Length", content_length.to_string())?;
            resp.insert_header("Connection", "close")?;

            session.write_response_header(Box::new(resp), false).await?;
            session
                .write_response_body(Some(key_auth_bytes), true)
                .await?;

            // Log this ACME challenge response for debugging
            info!(
                "ACME challenge completed: {} {} - 200 OK - {}ms",
                ctx.method,
                ctx.path,
                ctx.start_time.elapsed().as_millis()
            );

            // Update routing status for potential logging
            ctx.routing_status = "acme_challenge".to_string();

            return Ok(true);
        }

        // Check for redirects or static file serving
        if let Some((redirect_target, status_code)) = self
            .project_context_resolver
            .get_redirect_info(&ctx.host)
            .await
        {
            let location =
                domain_redirect_location(&redirect_target, &ctx.path, ctx.query_string.as_deref());
            let mut resp = ResponseHeader::build(status_code, None)?;
            resp.insert_header(header::LOCATION, &location)?;
            session.write_response_header(Box::new(resp), true).await?;
            return Ok(true);
        }

        // Check if this is a static deployment using route table. A path
        // route serves from its target instead: its files if the target is
        // static, the upstream otherwise.
        let static_dir = match ctx.path_route.as_ref() {
            Some(route) => route
                .target
                .as_ref()
                .and_then(|target| target.static_dir())
                .map(str::to_string),
            None => {
                self.project_context_resolver
                    .get_static_path(&ctx.host)
                    .await
            }
        };
        if let Some(static_dir) = static_dir {
            debug!(
                "Static deployment detected for {}: {}",
                ctx.host, static_dir
            );

            // IMPORTANT: Skip static file serving for /api/_temps/* paths
            // These must ALWAYS be proxied to the console address (admin API)
            if !ctx.path.starts_with("/api/_temps/") {
                let served = self.serve_static_target(session, ctx, &static_dir).await;
                match served {
                    Ok(StaticFileServeOutcome::Served) => {
                        debug!("Served static file: {}", ctx.path);
                        ctx.routing_status = "static_file".to_string();
                        self.log_static_request(ctx, 200, "static_file", &static_dir, None, None);
                        return Ok(true);
                    }
                    Ok(StaticFileServeOutcome::ServedNotFoundPage) => {
                        debug!(
                            request_path = %bounded_log_value(&ctx.path),
                            stored_static_dir = %bounded_log_value(&static_dir),
                            "Static file request answered with the deployment's 404 page"
                        );
                        ctx.routing_status = "static_file_not_found_page".to_string();
                        self.log_static_request(
                            ctx,
                            404,
                            "static_file_not_found_page",
                            &static_dir,
                            Some("Static file not found".to_string()),
                            None,
                        );
                        return Ok(true);
                    }
                    Ok(StaticFileServeOutcome::Redirected) => {
                        debug!(
                            request_path = %bounded_log_value(&ctx.path),
                            "Static file request redirected to its slashless page"
                        );
                        ctx.routing_status = "static_file_redirect".to_string();
                        self.log_static_request(
                            ctx,
                            308,
                            "static_file_redirect",
                            &static_dir,
                            None,
                            None,
                        );
                        return Ok(true);
                    }
                    Ok(StaticFileServeOutcome::NotFound) => {
                        debug!(
                            request_path = %bounded_log_value(&ctx.path),
                            stored_static_dir = %bounded_log_value(&static_dir),
                            "Static file request returned the uniform not-found response"
                        );
                        let contract = static_not_found_contract(&ctx.method);
                        let mut resp = ResponseHeader::build(contract.status, None)?;
                        resp.insert_header(header::CONTENT_TYPE, contract.content_type)?;
                        resp.insert_header(
                            header::CONTENT_LENGTH,
                            contract.content_length.to_string(),
                        )?;
                        resp.insert_header(header::CACHE_CONTROL, contract.cache_control)?;
                        resp.insert_header("X-Request-ID", &ctx.request_id)?;

                        self.ensure_static_visitor_session(session, ctx, contract.content_type)
                            .await;
                        self.set_tracking_cookies(session, &mut resp, ctx).await?;
                        session.write_response_header(Box::new(resp), false).await?;
                        if contract.send_body {
                            session
                                .write_response_body(
                                    Some(Bytes::from_static(STATIC_NOT_FOUND_BODY)),
                                    true,
                                )
                                .await?;
                        } else {
                            session.write_response_body(None, true).await?;
                        }

                        self.log_static_request(
                            ctx,
                            404,
                            "static_file_not_found",
                            &static_dir,
                            Some("Static file not found".to_string()),
                            Some(contract.content_length as i64),
                        );
                        return Ok(true);
                    }
                    Err(error) => return Err(error),
                }
            }
            // If we reach here and path starts with /api/_temps/,
            // fall through to normal proxying logic (will be proxied to console)
        }

        // Serve persisted static assets via deployment-prefixed URLs.
        // /_temps/assets/{deployment_slug}/path → DB lookup → CAS blob
        if ctx.path.starts_with("/_temps/assets/") {
            let after_prefix = &ctx.path["/_temps/assets/".len()..];
            if let Some(slash_pos) = after_prefix.find('/') {
                let deployment_slug = &after_prefix[..slash_pos];
                let asset_path = after_prefix[slash_pos + 1..].to_string();
                let mut legacy_source = None;
                let mut legacy_current_slug = None;
                let mut asset_scope = ctx.deployment.as_ref().and_then(|deployment| {
                    let context = deployment.context_vars.as_ref();
                    let source_slug = context
                        .and_then(|value| value.get("source_deployment_slug"))
                        .and_then(serde_json::Value::as_str);
                    let source_deployment_id = context
                        .and_then(|value| value.get("source_deployment_id"))
                        .and_then(serde_json::Value::as_i64)
                        .and_then(|value| i32::try_from(value).ok());
                    if source_slug.is_none() {
                        legacy_source = source_deployment_id;
                        legacy_current_slug = Some(deployment.slug.clone());
                    }
                    deployment_asset_scope(
                        &deployment.slug,
                        deployment.environment_id,
                        deployment.id,
                        source_slug,
                        context
                            .and_then(|value| value.get("source_environment_id"))
                            .and_then(serde_json::Value::as_i64)
                            .and_then(|value| i32::try_from(value).ok()),
                        source_deployment_id,
                        deployment_slug,
                    )
                });
                if let (Some(project_id), Some(source_deployment_id)) = (
                    ctx.project.as_ref().map(|project| project.id),
                    legacy_source,
                ) {
                    if let Some(origin) = self
                        .static_asset_lookup
                        .resolve_legacy_asset_origin(project_id, source_deployment_id)
                        .await
                    {
                        asset_scope = legacy_deployment_asset_scope(
                            legacy_current_slug.as_deref().unwrap_or_default(),
                            &origin,
                            deployment_slug,
                        );
                    }
                }
                if Self::is_cacheable_static_asset(&asset_path) {
                    if let Some(asset_scope) = asset_scope {
                        if let Ok(true) = self
                            .serve_asset_from_store(session, ctx, &asset_path, Some(asset_scope))
                            .await
                        {
                            ctx.routing_status = "prefixed_asset".to_string();
                            return Ok(true);
                        }
                    }
                }
            }
        }

        // Fallback: serve immutable static assets from file store.
        // For container deployments where the upstream didn't have the asset,
        // check the path-keyed file store (stale-chunk fallback).
        if Self::is_cacheable_static_asset(&ctx.path)
            && ctx.path_route.is_none()
            && !self
                .project_context_resolver
                .is_static_deployment(&ctx.host)
                .await
        {
            let url_path = ctx.path.trim_start_matches('/').to_string();
            if let Ok(true) = self
                .serve_asset_from_store(session, ctx, &url_path, None)
                .await
            {
                ctx.routing_status = "stale_chunk_fallback".to_string();
                return Ok(true);
            }
        }

        // Admin gate: when a non-noop gate is wired and the request is
        // about to fall back to the console (no deployed app for this host,
        // not a public ingest path under /api/_temps/*, not a preview),
        // require the (IP, Host) tuple to pass the gate. If it doesn't, the
        // management surface must stay invisible from non-admin hosts.
        //
        // The public ingress hostname (ADR-051) is not gated at all: the
        // resolver pins it to the public-only listener for every client, so
        // there is no management surface behind it to protect.
        if let Some(gate) = self
            .admin_gate
            .as_ref()
            .filter(|_| !self.is_public_ingress_request(&ctx.host))
        {
            let config = gate.current();
            if Self::should_consult_admin_gate(&config, &ctx.path, ctx.preview_route.is_some()) {
                let host_has_route = self.upstream_resolver.has_route_for_host(&ctx.host).await;
                if !host_has_route {
                    let client_ip = ctx
                        .ip_address
                        .as_deref()
                        .and_then(|s| s.parse::<std::net::IpAddr>().ok())
                        .unwrap_or_else(|| std::net::IpAddr::from([127, 0, 0, 1]));
                    match Self::console_fall_through(
                        &config,
                        client_ip,
                        &ctx.host,
                        self.upstream_resolver.console_ingress_address().is_some(),
                    ) {
                        ConsoleFallThrough::Console => {}
                        // A denied client is still a legitimate caller of the
                        // public routes: a mail client fetching a tracking
                        // pixel, a git provider delivering a push. Hand it to
                        // the listener that mounts those and nothing else.
                        // That listener answers everything that is not a
                        // public route with a 404, so the console stays as
                        // invisible as when the proxy answered itself.
                        ConsoleFallThrough::PublicSurfaceOnly => {
                            debug!(
                                host = %ctx.host,
                                client_ip = %client_ip,
                                path = %ctx.path,
                                "admin gate denied request; serving the public-only surface"
                            );
                            ctx.public_surface_only = true;
                        }
                        ConsoleFallThrough::NotFound => {
                            warn!(
                                host = %ctx.host,
                                client_ip = %client_ip,
                                path = %ctx.path,
                                "admin gate denied request to non-admin host"
                            );
                            let mut response = ResponseHeader::build(StatusCode::NOT_FOUND, None)?;
                            response.insert_header("Cache-Control", "no-store")?;
                            response.insert_header("X-Request-ID", &ctx.request_id)?;
                            response.insert_header("Content-Type", "text/html; charset=utf-8")?;
                            let body =
                                Bytes::from(crate::branded_404::render(&ctx.host, &ctx.request_id));
                            response.insert_header("Content-Length", body.len().to_string())?;
                            session
                                .write_response_header(Box::new(response), false)
                                .await?;
                            session.write_response_body(Some(body), true).await?;
                            ctx.routing_status = "admin_gate_denied".to_string();
                            return Ok(true);
                        }
                    }
                }
            }
        }

        Ok(false)
    }

    async fn upstream_response_filter(
        &self,
        session: &mut PingoraSession,
        upstream_response: &mut ResponseHeader,
        ctx: &mut Self::CTX,
    ) -> Result<()>
    where
        Self::CTX: Send + Sync,
    {
        {
            let key = upstream_failure_key(
                ctx.project.as_ref().map(|p| p.id),
                ctx.environment.as_ref().map(|e| e.id),
                ctx.upstream_host.as_deref(),
                &ctx.host,
            );
            // Only an actual upstream response proves recovery. Selecting a
            // route or starting a retry must not re-arm the failure warning.
            if let Some(failures) = self.upstream_failures.record_success(&key) {
                info!(
                    project_id = key.0,
                    environment_id = key.1,
                    failures,
                    "Upstream connection recovered"
                );
            }
        }

        debug!("Upstream response filter headers: {:?}", upstream_response);

        strip_proxy_owned_response_headers(upstream_response);
        if ctx.gated_host {
            crate::handler::gate::strip_gate_from_app_response(upstream_response)?;
        }
        upstream_response.insert_header("X-Temps-Proxy-Probe-Capable", "1")?;

        // First upstream header = backend latency (connect + upstream time).
        if ctx.upstream_response_time_ms.is_none() {
            if let Some(start) = ctx.upstream_start_time {
                ctx.upstream_response_time_ms = Some(start.elapsed().as_millis() as u64);
            }
        }

        ctx.upstream_response_headers = Some(upstream_response.clone());

        let headers_map: HashMap<String, String> = upstream_response
            .headers
            .iter()
            .filter_map(|(k, v)| v.to_str().ok().map(|val| (k.to_string(), val.to_string())))
            .collect();
        ctx.response_headers = Some(headers_map.clone());

        // Detect SSE by content-type header from upstream
        let is_sse = upstream_response
            .headers
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .map(is_event_stream_content_type)
            .unwrap_or(false);

        if is_sse {
            ctx.is_sse = true;
            // Skip visitor/session tracking for SSE streams
            ctx.skip_tracking = true;
            // The upstream *confirmed* a stream, as opposed to `ctx.is_sse` set
            // from the request's Accept header, which is only client intent and
            // may still be answered by an ordinary short response.
            ctx.streaming_session = true;
            debug!("SSE response detected from upstream");
        }

        // Strip content-length from HEAD responses, but ONLY when the downstream
        // client is on HTTP/2. The upstream correctly includes it (per RFC 9110
        // §9.3.2, HEAD responses SHOULD have the same content-length as GET) --
        // over HTTP/2, clients like curl interpret the content-length as a promise
        // of body bytes and error when none arrive, and Cloudflare strips it too.
        // But an HTTP/1.1 downstream needs content-length (or chunked encoding) on
        // a keep-alive connection to know the response is complete; a HEAD response
        // with neither leaves the client blocked waiting for a body that will never
        // come, since HTTP/1.1 has no other framing signal for "zero-length body,
        // connection stays open."
        if ctx.method == "HEAD" && session.is_http2() {
            upstream_response.remove_header("content-length");
        }

        // Add X-Served-By header with the container name that handled this request
        if let Some(name) = &ctx.container_name {
            upstream_response.insert_header("X-Served-By", name).ok();
        }

        // Confirm or cancel Markdown conversion now that we know the upstream status and
        // content type.  We only convert successful (2xx) text/html responses; everything
        // else passes through unchanged so the client receives the original response as-is.
        apply_markdown_upstream_gate(upstream_response, ctx);
        if !ctx.wants_markdown {
            if let Some(accept_encoding) = ctx.markdown_fallback_accept_encoding.take() {
                let no_body =
                    ctx.method == "HEAD" || matches!(upstream_response.status.as_u16(), 204 | 304);
                restore_client_compression(
                    &mut session.upstream_compression,
                    &accept_encoding,
                    upstream_response,
                    no_body,
                );
            }
        }

        Ok(())
    }

    /// Accumulate request body bytes as they stream in. The only reliable
    /// way to measure upload size — chunked-encoded request bodies carry no
    /// `Content-Length` header and would otherwise log as 0 (see `log_request`).
    async fn request_body_filter(
        &self,
        _session: &mut PingoraSession,
        body: &mut Option<Bytes>,
        _end_of_stream: bool,
        ctx: &mut Self::CTX,
    ) -> Result<()>
    where
        Self::CTX: Send + Sync,
    {
        if let Some(chunk) = body.as_ref() {
            ctx.client_body_bytes_received += chunk.len();
        }
        Ok(())
    }

    /// Thin wrapper over `response_body_filter_inner` that accumulates the
    /// bytes actually forwarded to the client on every exit path (passthrough,
    /// SSE/WebSocket, and the buffered Markdown conversion below). Chunked
    /// responses carry no `Content-Length` header, so this accumulated count
    /// is the only reliable source for response bandwidth (see `log_request`).
    fn response_body_filter(
        &self,
        _session: &mut PingoraSession,
        body: &mut Option<Bytes>,
        end_of_stream: bool,
        ctx: &mut Self::CTX,
    ) -> Result<Option<std::time::Duration>>
    where
        Self::CTX: Send + Sync,
    {
        let result = response_body_filter_inner(body, end_of_stream, ctx);
        if let Some(chunk) = body.as_ref() {
            ctx.upstream_body_bytes_received += chunk.len();
        }
        result
    }

    async fn response_filter(
        &self,
        session: &mut PingoraSession,
        upstream_response: &mut ResponseHeader,
        ctx: &mut Self::CTX,
    ) -> Result<()>
    where
        Self::CTX: Send + Sync,
    {
        if let Some(version) =
            downstream_response_version(session.req_header().version, upstream_response.version)
        {
            upstream_response.set_version(version);
        }

        // `X-Temps-Shield` says what Shield did (ADR 066, D11): only Shield
        // sets it, and only when it acted.
        let _ = upstream_response.remove_header(temps_shield::RESPONSE_HEADER);
        if let Some(verdict) = ctx.shield_header {
            upstream_response.insert_header(
                temps_shield::RESPONSE_HEADER,
                temps_shield::response_header_value(verdict, &ctx.request_id),
            )?;
        }

        // Capture upstream write pending time for upload diagnostics (Pingora 0.8.0)
        let pending_time = session.upstream_write_pending_time();
        if !pending_time.is_zero() {
            ctx.upstream_write_pending_time_ms = Some(pending_time.as_millis() as i32);
        }

        // Store content type for later use
        ctx.content_type = Some(
            upstream_response
                .headers
                .get("content-type")
                .and_then(|h| h.to_str().ok())
                .unwrap_or_default()
                .to_string(),
        );

        // Rewrite response headers for Markdown conversion.
        // We must do this here (before the body arrives) because Pingora sends headers
        // to the client before calling response_body_filter.
        apply_markdown_response_headers(upstream_response, ctx);

        // Detect chunked transfer encoding in response
        let is_chunked_response = upstream_response
            .headers
            .get("transfer-encoding")
            .and_then(|v| v.to_str().ok())
            .map(|te| te.contains("chunked"))
            .unwrap_or(false);

        // For chunked responses, ensure Transfer-Encoding is preserved
        if is_chunked_response {
            debug!("Chunked transfer encoding response detected - preserving for streaming");
            debug!(
                "Current headers before preservation: {:?}",
                upstream_response.headers.get_all("transfer-encoding")
            );
            debug!(
                "Content-Encoding header: {:?}",
                upstream_response.headers.get("content-encoding")
            );

            // Ensure Transfer-Encoding header is present and set to chunked
            // This tells Pingora and the client that the response is streamed in chunks
            if !upstream_response.headers.contains_key("transfer-encoding") {
                upstream_response.insert_header("Transfer-Encoding", "chunked")?;
            }
        }

        // Handle SSE (Server-Sent Events) special headers
        if ctx.is_sse {
            // Ensure required SSE headers are present for proper streaming
            if !upstream_response.headers.contains_key("cache-control") {
                upstream_response.insert_header("Cache-Control", "no-cache")?;
            }
            if !upstream_response.headers.contains_key("connection") {
                upstream_response.insert_header("Connection", "keep-alive")?;
            }
            if !upstream_response.headers.contains_key("x-accel-buffering") {
                upstream_response.insert_header("X-Accel-Buffering", "no")?;
            }

            debug!(
                "SSE stream response for path={}, setting streaming headers",
                ctx.path
            );

            // Skip visitor tracking and session creation for SSE
            ctx.skip_tracking = true;
        }

        // Handle WebSocket upgrade responses
        if ctx.is_websocket {
            // WebSocket requires specific upgrade headers - don't modify them
            debug!(
                "WebSocket upgrade response for path={}, preserving upgrade headers",
                ctx.path
            );

            // Skip visitor tracking and session creation for WebSocket
            ctx.skip_tracking = true;
        }

        // Determine if this needs visitor tracking
        let status_code = upstream_response.status.as_u16();
        let request_accept = ctx
            .request_headers
            .as_ref()
            .and_then(|headers| headers.get("accept"))
            .map(String::as_str);
        let fetch_destination = ctx
            .request_headers
            .as_ref()
            .and_then(|headers| headers.get("sec-fetch-dest"))
            .map(String::as_str);
        let upgrade_insecure_requests = ctx
            .request_headers
            .as_ref()
            .and_then(|headers| headers.get("upgrade-insecure-requests"))
            .map(String::as_str);

        // Check if we should track this page view
        let should_track = Self::should_track_page(
            &ctx.path,
            ctx.content_type.as_deref(),
            &ctx.method,
            request_accept,
            fetch_destination,
            upgrade_insecure_requests,
        );

        // Only browser HTML documents create visitor/session state (skip SSE,
        // API responses, assets, and other non-document traffic).
        if !ctx.skip_tracking && should_track {
            self.ensure_visitor_session(ctx).await;
        } else {
            debug!(
                "Skipping visitor creation for: path={}, content_type={:?}, status={}, skip_tracking={}",
                ctx.path, ctx.content_type, status_code, ctx.skip_tracking
            );
        }

        // Finalize the response
        if let Err(e) = self
            .finalize_response(session, upstream_response, ctx)
            .await
        {
            error!("Failed to finalize response: {:?}", e);
            return Err(Error::new_str("Failed to finalize response"));
        }

        Ok(())
    }

    async fn upstream_request_filter(
        &self,
        _session: &mut PingoraSession,
        upstream_request: &mut RequestHeader,
        ctx: &mut Self::CTX,
    ) -> Result<()>
    where
        Self::CTX: Send + Sync,
    {
        // ADR 065 D8: no app receives the gate's cookies, admitted or not,
        // gated host or not. A cookie left from before an address stopped
        // being signed-in is still a token.
        crate::handler::gate::strip_gate_cookies(upstream_request)?;
        // A stripping path route forwards `/api/users` as `/users` and tells
        // the target where it is mounted, for apps that build absolute URLs.
        // Only the platform sets that header on routed requests: a client
        // could otherwise choose the mount path the target believes in.
        if ctx.path_route.is_some() {
            upstream_request.remove_header("X-Forwarded-Prefix");
        }
        if let Some(route) = ctx.path_route.as_ref().filter(|route| route.strip_prefix) {
            let original = upstream_request
                .uri
                .path_and_query()
                .map_or("/", |path_and_query| path_and_query.as_str());
            let rewritten = route.upstream_path(original);
            let uri = rewritten.parse::<axum::http::Uri>().map_err(|e| {
                Error::because(
                    pingora::ErrorType::InternalError,
                    "path route produced an invalid upstream URI",
                    e,
                )
            })?;
            upstream_request.set_uri(uri);
            upstream_request.insert_header("X-Forwarded-Prefix", route.prefix.as_str())?;
        }
        Ok(())
    }

    async fn upstream_peer(
        &self,
        session: &mut PingoraSession,
        ctx: &mut Self::CTX,
    ) -> Result<Box<HttpPeer>> {
        // Backend-latency basis. On connect retries this is re-stamped, so the
        // metric measures the attempt that actually served the response.
        ctx.upstream_start_time = Some(Instant::now());

        // WebSocket upgrades legitimately sit silent for minutes (idle
        // terminals, push-only feeds). Cap them at 1h instead of the 60s
        // default that HTTP uses, otherwise Pingora RSTs the socket every
        // minute when no bytes flow.
        let is_websocket = session
            .req_header()
            .headers
            .get("upgrade")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.eq_ignore_ascii_case("websocket"))
            .unwrap_or(false);

        // Workspace preview gateway: skip the route table and forward straight
        // to the local gateway. The host header is preserved so the gateway
        // can decode `ws-<sid>-<port>` and pick the right sandbox container.
        // This peer is an internal shared gateway, not customer app traffic,
        // so it keeps the fixed WS/HTTP split rather than the configurable
        // per-project/environment timeouts resolved below for the customer
        // traffic path.
        //
        // Every preview target shares this same physical peer address, so
        // `group_key` MUST be set per request — otherwise Pingora's
        // connection pool considers all sandboxes' requests interchangeable
        // and can hand a connection opened for one sandbox back out to
        // serve another token-bearing request (see `preview_request_group_key`
        // doc comment for the full mechanism).
        // A status page's custom hostname (#1412): the public-only listener
        // and nothing else. `request_filter` already refused the request if no
        // such listener exists; this keeps the guarantee if it ever did not.
        if ctx.status_page_host {
            let Some(ingress_address) = self.upstream_resolver.console_ingress_address() else {
                return Err(Error::explain(
                    pingora_core::ErrorType::ConnectNoRoute,
                    "status-page custom hostname has no public ingress listener to reach",
                ));
            };
            let mut peer = Box::new(HttpPeer::new(ingress_address, false, String::new()));
            peer.options.connection_timeout = Some(std::time::Duration::from_secs(5));
            peer.options.read_timeout = Some(std::time::Duration::from_secs(30));
            ctx.upstream_host = Some(ingress_address.to_string());
            return Ok(peer);
        }

        if let Some(host) = &ctx.preview_route {
            let preview_io_timeout = if is_websocket {
                std::time::Duration::from_secs(3600)
            } else {
                std::time::Duration::from_secs(60)
            };
            let gateway_peer = if ctx.preview_on_worker {
                // A sandbox on a worker is reached through the relay the
                // sandbox plugin runs in the console process; a proxy
                // running on its own has none.
                let Some(relay) = temps_core::sandbox_preview_relay::worker_preview_relay() else {
                    return Err(Error::explain(
                        pingora_core::ErrorType::ConnectNoRoute,
                        "this sandbox runs on a worker node, and its previews are relayed by \
                         the console process; this proxy runs without one",
                    ));
                };
                relay.address.to_string()
            } else {
                preview_gateway_peer()
            };
            let mut peer = Box::new(HttpPeer::new(gateway_peer.as_str(), false, String::new()));
            peer.group_key = preview_request_group_key(host, &ctx.request_id);
            peer.options.connection_timeout = Some(std::time::Duration::from_secs(5));
            peer.options.read_timeout = Some(preview_io_timeout);
            peer.options.write_timeout = Some(preview_io_timeout);
            peer.options.idle_timeout = Some(if is_websocket {
                preview_io_timeout
            } else {
                std::time::Duration::from_millis(1)
            });
            ctx.upstream_host = Some(gateway_peer);
            return Ok(peer);
        }

        let domain = self.get_host_header(session)?;
        let path = session.req_header().uri.path().to_string();

        debug!(
            "Resolving upstream peer for domain: {}, path: {}",
            domain, path
        );

        // The admin gate denied this request: it may reach the console's
        // public routes and nothing else (ADR-051). Skip resolution entirely
        // so no path or `Host` can steer it to another peer. This takes
        // precedence over a path route's target for the same reason.
        let public_surface_address = ctx
            .public_surface_only
            .then(|| self.upstream_resolver.console_ingress_address())
            .flatten();
        // A path route's target was resolved in `request_filter`; the
        // request goes to its current deployment, not the host's service.
        let path_target = if public_surface_address.is_some() {
            None
        } else {
            ctx.path_route
                .as_ref()
                .and_then(|route| route.target.as_ref())
        };
        let selection = if let Some(ingress_address) = public_surface_address {
            PeerSelection {
                peer: Box::new(HttpPeer::new(ingress_address, false, String::new())),
                container_id: None,
                container_name: None,
            }
        } else if let Some(target) = path_target {
            // Static targets are answered in `request_filter`; reaching here
            // with one would otherwise fall back to the console address.
            if target.is_static() {
                return Err(Error::explain(
                    pingora::ErrorType::HTTPStatus(502),
                    "path route target is static and has no upstream",
                ));
            }
            let backend = target.select_backend();
            PeerSelection {
                peer: Box::new(HttpPeer::new(
                    backend.address.as_str(),
                    false,
                    String::new(),
                )),
                container_id: backend.container_id,
                container_name: backend.container_name,
            }
        } else {
            // Use the upstream resolver trait
            // Pass SNI hostname for TLS-based routing
            self.upstream_resolver
                .resolve_peer_for_request(
                    &domain,
                    &path,
                    session.req_header().method.as_str(),
                    ctx.sni_hostname.as_deref(),
                )
                .await?
        };

        let mut peer = selection.peer;

        // The console failed to start: nothing is listening on its address
        // (or something unrelated is). Don't connect; `fail_to_proxy` serves
        // the startup-failure page. One atomic load on the healthy path.
        if let Some(responder) = self.console_unavailable.as_ref() {
            if responder.phase() == temps_core::console_startup::ConsolePhase::Failed
                && responder.is_console_peer(&peer)
            {
                ctx.upstream_host = Some(peer.address().to_string());
                return Err(Error::explain(
                    pingora::ErrorType::ConnectRefused,
                    "console failed to start; serving its startup status page",
                ));
            }
        }

        // Resolve the effective per-request/idle timeout for customer app
        // traffic: project config as the base layer, environment config
        // overriding it (Environment > Project > Global — the same
        // inheritance chain used elsewhere, e.g. for security config), then
        // always clamped to the operator's global hard ceiling. SSE and
        // WebSocket get their own idle-timeout class since they're
        // long-lived by design; `ctx.is_sse`/`ctx.is_websocket` were already
        // detected from request headers in `early_request_filter`.
        let request_timeouts = self
            .config_service
            .get_settings()
            .await
            .map(|settings| settings.request_timeouts)
            .unwrap_or_default();
        let project_config = ctx
            .project
            .as_ref()
            .and_then(|p| p.deployment_config.clone())
            .unwrap_or_default();
        let effective_config = ctx
            .environment
            .as_ref()
            .map(|env| env.get_effective_deployment_config(&project_config))
            .unwrap_or(project_config);
        let traffic_kind = if ctx.is_websocket {
            TimeoutTrafficKind::WebSocket
        } else if ctx.is_sse {
            TimeoutTrafficKind::Sse
        } else {
            TimeoutTrafficKind::Http
        };
        let customer_io_timeout =
            resolve_customer_io_timeout(traffic_kind, &effective_config, &request_timeouts);

        // The customer-traffic timeout above is tuned per project/environment
        // — a slow customer endpoint shouldn't hang a proxy worker forever.
        // It's the wrong bound for the console/control-plane API, which the
        // browser reaches through this same proxy: long-running admin
        // operations (e.g. POST /api/imports/execute, which synchronously
        // builds, deploys, and health-checks the imported app) routinely
        // take well over 60s for a real app. Without this, the request is
        // RST'd out from under a handler that goes on to finish successfully
        // server-side — the import completes, but the browser sees a 503
        // and the user has no way to know it worked.
        let peer_address = peer.address().to_string();
        let io_timeout = upstream_io_timeout(
            &peer_address,
            control_plane_address(
                &peer_address,
                self.upstream_resolver.console_address(),
                self.upstream_resolver.console_ingress_address(),
            ),
            is_websocket,
            customer_io_timeout,
        );

        // Configure upstream connection options. `io_timeout` is the
        // project/environment-configured (or global-default) value for this
        // traffic's class — HTTP/SSE/WebSocket — resolved above, bumped to
        // `CONSOLE_IO_TIMEOUT_SECS` for console/control-plane traffic (see
        // above). `None` means no timeout is configured for this traffic at
        // all (the platform default) and flows straight through to Pingora,
        // which leaves the connection unbounded — never converted to "the
        // ceiling" or any other fallback duration.
        peer.options.connection_timeout = Some(std::time::Duration::from_secs(5));
        peer.options.read_timeout = io_timeout;
        peer.options.write_timeout = io_timeout;
        // Close idle pooled connections after the same window to avoid stale
        // keep-alive reuse.
        peer.options.idle_timeout = io_timeout;

        // Populate context with upstream information
        let addr = peer.address();
        ctx.upstream_host = Some(addr.to_string());

        // Set container info from the upstream resolver's backend selection
        if selection.container_id.is_some() {
            ctx.container_id = selection.container_id;
            ctx.container_name = selection.container_name;
        } else if let Some(deployment) = &ctx.deployment {
            ctx.container_id = Some(format!("deployment-{}", deployment.id));
        }

        Ok(peer)
    }

    fn fail_to_connect(
        &self,
        _session: &mut PingoraSession,
        _peer: &HttpPeer,
        ctx: &mut Self::CTX,
        mut e: Box<Error>,
    ) -> Box<Error> {
        // Retry once on connection failure — handles stale pooled connections
        // where the upstream closed the keep-alive connection before we sent
        // the request (TCP RST / "Connection reset by peer").
        //
        // Neither branch logs above debug: a final failure reaches
        // `fail_to_proxy`, which logs it once with the request context.
        if ctx.upstream_connect_tries == 0 {
            ctx.upstream_connect_tries += 1;
            debug!("Upstream connection failed (try 1), retrying: {:?}", e);
            e.set_retry(true);
        } else {
            debug!("Upstream connection failed after retry: {:?}", e);
        }
        e
    }

    /// `fail_to_proxy` already logs every failure once, with the request
    /// context and a level that matches the cause. Pingora's own
    /// "Fail to proxy" ERROR line would log each of them a second time.
    fn suppress_error_log(
        &self,
        _session: &PingoraSession,
        _ctx: &Self::CTX,
        _error: &Error,
    ) -> bool {
        true
    }

    async fn fail_to_proxy(
        &self,
        session: &mut PingoraSession,
        e: &Error,
        ctx: &mut Self::CTX,
    ) -> FailToProxy
    where
        Self::CTX: Send + Sync,
    {
        if let Some(result) = self.serve_console_status_page(session, e, ctx).await {
            return result;
        }

        let failure = ProxyFailureKind::of(e);
        macro_rules! log_failure {
            ($level:ident) => {
                $level!(
                    "Failed to proxy ({}): {:?} | request_id={} client_ip={} host={} method={} path={}",
                    failure.as_str(),
                    e,
                    ctx.request_id,
                    ctx.ip_address.as_deref().unwrap_or("unknown"),
                    ctx.host,
                    ctx.method,
                    ctx.path
                )
            };
        }
        match failure {
            ProxyFailureKind::ClientGone => log_failure!(debug),
            ProxyFailureKind::Upstream => {
                let key = upstream_failure_key(
                    ctx.project.as_ref().map(|p| p.id),
                    ctx.environment.as_ref().map(|e| e.id),
                    ctx.upstream_host.as_deref(),
                    &ctx.host,
                );
                if self
                    .upstream_failures
                    .record_failure_bounded(key, 4096)
                    .should_log()
                {
                    log_failure!(warn);
                } else {
                    log_failure!(debug);
                }
            }
            ProxyFailureKind::Internal => log_failure!(error),
        }

        let mut error_code = 500;
        let can_reuse_downstream = false;

        // Update context with error
        ctx.error_message = Some(e.to_string());
        ctx.routing_status = "error".to_string();

        // The client already went away: there is nobody to send a 503 page
        // to, and trying only produces a second write error. Record it as
        // 499 (client closed request) and report that no response was sent.
        if failure == ProxyFailureKind::ClientGone {
            self.log_failed_request(ctx, CLIENT_CLOSED_REQUEST, None);
            return FailToProxy {
                error_code: 0,
                can_reuse_downstream,
            };
        }

        let mut header = match ResponseHeader::build(503, None) {
            Ok(header) => header,
            Err(e) => {
                error!("Failed to build response header: {:?}", e);
                return FailToProxy {
                    error_code,
                    can_reuse_downstream,
                };
            }
        };

        if let Err(e) = header.insert_header(header::SERVER, &SERVER_NAME[..]) {
            error!("Failed to insert SERVER header: {:?}", e);
        }
        if let Err(e) = header.insert_header(header::DATE, "Sun, 06 Nov 1994 08:49:37 GMT") {
            error!("Failed to insert DATE header: {:?}", e);
        }
        if let Err(e) = header.insert_header(header::CACHE_CONTROL, "private, no-store") {
            error!("Failed to insert CACHE_CONTROL header: {:?}", e);
        }
        if let Err(e) = header.insert_header("content-type", "text/html; charset=utf-8") {
            error!("Failed to insert content-type header: {:?}", e);
        }

        if let Err(e) = session.write_response_header(Box::new(header), false).await {
            error!("Failed to write response header: {:?}", e);
            return FailToProxy {
                error_code,
                can_reuse_downstream,
            };
        }

        let page = unavailable_page(ctx.preview_route.is_some());
        if let Err(e) = session
            .write_response_body(Some(Bytes::from(page)), true)
            .await
        {
            error!("Failed to write response body: {:?}", e);
        }

        error_code = 503;
        self.log_failed_request(ctx, error_code, Some(page.len() as i64));

        FailToProxy {
            error_code,
            can_reuse_downstream,
        }
    }

    /// End-of-request hook — Pingora calls this exactly once for EVERY
    /// request, whether it was proxied, served directly from `request_filter`
    /// (redirects, password walls, ACME challenges, static files), or failed.
    /// This is therefore the single record site for hot-path metrics, which
    /// guarantees the destination counters sum to `proxy.requests`.
    async fn logging(&self, session: &mut PingoraSession, _e: Option<&Error>, ctx: &mut Self::CTX)
    where
        Self::CTX: Send + Sync,
    {
        // ctx.connection_permit (if any) releases its slot when ctx is dropped
        // after this hook returns — no explicit release needed here.

        // No response written (client abort / connect failure with no reply)
        // has no status; 0 falls into the 5xx class, which is the honest read.
        let status_code = session
            .response_written()
            .map(|resp| resp.status.as_u16())
            .unwrap_or(0);

        let destination = crate::metrics::RequestDestination::classify(
            ctx.project.is_some(),
            &ctx.routing_status,
        );

        // A `101` means the WebSocket tunnel was actually established, so this
        // hook is firing at tunnel *close* — anything up to the 1h idle timeout
        // set in `upstream_peer`. Together with an upstream-confirmed SSE
        // stream these are the two cases where `start_time.elapsed()` is a
        // connection lifetime rather than a request latency.
        let is_streaming = status_code == 101 || ctx.streaming_session;

        // Adaptive difficulty (ADR 066, D6) compares each environment's 5xx
        // ratio with its last hour.
        if let Some(env_id) = ctx.shield_env {
            self.shield
                .engine
                .record_status(env_id, status_code, temps_shield::unix_now());
        }
        if let Some(mut line) = ctx.netwatch_line.take() {
            line.status = status_code;
            self.shield.netwatch.send(line);
        }

        // Hot path: a handful of relaxed atomic adds, no locks, no I/O.
        self.proxy_metrics.record(
            status_code,
            ctx.start_time.elapsed().as_millis() as u64,
            ctx.upstream_response_time_ms,
            destination,
            is_streaming,
        );

        // The response body has now fully streamed through response_body_filter
        // (this hook fires in Pingora's finish(), after every body task), so
        // upstream_body_bytes_received holds the real byte count. Patch it into
        // the entry log_request stashed at header-time and send it now — this
        // is the only place a proxied response's byte count is accurate.
        if let Some(mut pending) = ctx.pending_proxy_log.take() {
            if ctx.upstream_body_bytes_received > 0 {
                pending.response_size_bytes = Some(ctx.upstream_body_bytes_received as i64);
            }
            self.proxy_log_handle.send_or_drop(pending);
        }
    }
}
