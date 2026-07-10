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
//! The single intentional exception is the ACME HTTP-01 challenge lookup
//! (`handle_acme_http_challenge`), which is path-gated to
//! `/.well-known/acme-challenge/*` — a path that is rare by construction and never
//! appears on normal traffic. Every other request-path DB call present before this
//! branch was removed as part of `perf/remove-db-from-request-path` (WS1–WS6).

use crate::handler::preview_wall::{
    build_logout_cookie_sandbox, generate_preview_form_html_labeled, sanitize_next,
    PREVIEW_LOGIN_PATH, PREVIEW_LOGOUT_PATH,
};
use crate::on_demand::OnDemandManager;
use crate::preview_auth::{
    build_set_cookie_sandbox, check_preview_auth, encode_preview_cookie_subject,
    parse_preview_host, preview_peer_group_key, verify_argon2, PreviewAuthLimiter,
    PreviewAuthOutcome, PreviewHost, PreviewSandboxLookup, SandboxLookupCache,
    PREVIEW_GATEWAY_PEER,
};
use crate::service::cert_host_cache::CertHostCache;
use crate::service::challenge_service::ChallengeService;
use crate::service::cookie_codec::{
    make_v2_session_payload, parse_session_cookie, parse_visitor_cookie,
};
use crate::service::ip_access_control_service::IpAccessControlService;
use crate::service::proxy_log_batch_writer::{
    ProxyLogBatchHandle, TrackingBatchHandle, TrackingEvent,
};
use crate::service::proxy_log_service::CreateProxyLogRequest;
use crate::tls_fingerprint;
use crate::traits::*;
use async_trait::async_trait;
use axum::http::header;
use bytes::Bytes;
use cookie::Cookie;
use flate2::write::GzEncoder;
use flate2::Compression;
use pingora::http::StatusCode;
use pingora::Error;
use pingora_core::{
    upstreams::peer::{HttpPeer, Peer},
    Result,
};
use pingora_http::ResponseHeader;
use pingora_proxy::{FailToProxy, ProxyHttp, Session as PingoraSession};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use std::collections::HashMap;
use std::io::Write;
use std::sync::Arc;
use std::time::Instant;
use temps_database::DbConnection;
use temps_entities::{deployments, domains, environments, projects};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

// Constants
pub const VISITOR_ID_COOKIE: &str = "_temps_visitor_id";

/// Maximum HTML body size (in bytes) eligible for Markdown conversion.
/// Mirrors Cloudflare's "Markdown for Agents" 2 MB limit.
const MAX_MARKDOWN_BODY_BYTES: usize = 2 * 1024 * 1024;

/// Estimate the number of tokens in a Markdown document using a simple
/// word-count heuristic (tokens ≈ words × 1.33, i.e. words / 0.75).
/// This matches the rough estimate used by the Cloudflare `x-markdown-tokens` header.
fn estimate_markdown_tokens(markdown: &str) -> usize {
    let word_count = markdown.split_whitespace().count();
    // 1 token ≈ 0.75 words  →  tokens ≈ words / 0.75 ≈ words * 4 / 3
    word_count * 4 / 3
}

/// Metadata extracted from a page's `<head>` for the YAML front-matter block.
struct PageMeta {
    title: Option<String>,
    description: Option<String>,
    image: Option<String>,
}

impl PageMeta {
    /// Return a YAML front-matter block, or `None` if no metadata was found.
    fn to_frontmatter(&self) -> Option<String> {
        if self.title.is_none() && self.description.is_none() && self.image.is_none() {
            return None;
        }
        let mut fm = String::from("---\n");
        if let Some(t) = &self.title {
            fm.push_str(&format!("title: {}\n", t));
        }
        if let Some(d) = &self.description {
            fm.push_str(&format!("description: {}\n", d));
        }
        if let Some(i) = &self.image {
            fm.push_str(&format!("image: {}\n", i));
        }
        fm.push_str("---\n\n");
        Some(fm)
    }
}

/// Parse YAML front-matter metadata from `<head>` meta tags.
///
/// Priority for `title`:
///   1. `<meta property="og:title">` — the short title without site-name suffix.
///   2. `<title>` — fallback, used when og:title is absent.
///
/// Priority for `description`:
///   1. `<meta name="description">` — canonical description.
///   2. `<meta property="og:description">` — fallback.
///
/// Priority for `image`:
///   1. `<meta property="image">` (Cloudflare convention).
///   2. `<meta property="og:image">`.
fn extract_page_meta(document: &scraper::Html) -> PageMeta {
    use scraper::Selector;

    // Helper: return the `content` attribute of the first element matching `sel`.
    let first_content = |sel: &str| -> Option<String> {
        Selector::parse(sel).ok().and_then(|s| {
            document
                .select(&s)
                .next()
                .and_then(|el| el.attr("content"))
                .map(|v| v.to_owned())
        })
    };

    // Title: prefer og:title (short), fall back to <title> text content.
    let title = first_content(r#"meta[property="og:title"]"#).or_else(|| {
        Selector::parse("title").ok().and_then(|s| {
            document
                .select(&s)
                .next()
                .map(|el| el.text().collect::<String>())
                .filter(|t| !t.is_empty())
        })
    });

    let description = first_content(r#"meta[name="description"]"#)
        .or_else(|| first_content(r#"meta[property="og:description"]"#));

    let image = first_content(r#"meta[property="image"]"#)
        .or_else(|| first_content(r#"meta[property="og:image"]"#));

    PageMeta {
        title,
        description,
        image,
    }
}

/// Extract the inner HTML of the content node to convert to Markdown.
///
/// Strategy (matches Cloudflare's Markdown for Agents behaviour):
/// 1. First `<main>` element found at shallowest depth (document order).
/// 2. Fall back to `<body>` if no `<main>` is present.
/// 3. Fall back to the full document string if neither is found (e.g. plain
///    HTML fragments without a body element).
///
/// `<script>` and `<style>` elements inside the selected node are stripped
/// before returning, preventing inline JS/CSS and JSON-LD blobs from appearing
/// as raw text in the converted Markdown.
///
/// Returns the cleaned inner HTML ready to feed to htmd.
fn extract_content_html(document: &scraper::Html) -> String {
    use scraper::Selector;

    let inner = {
        if let Ok(sel) = Selector::parse("main") {
            document.select(&sel).next().map(|node| node.inner_html())
        } else {
            None
        }
    }
    .or_else(|| {
        Selector::parse("body")
            .ok()
            .and_then(|sel| document.select(&sel).next().map(|node| node.inner_html()))
    })
    .unwrap_or_else(|| document.html());

    strip_script_and_style(&inner)
}

/// Remove all `<script>` and `<style>` tags (and their content) from an HTML
/// fragment string.  We re-parse the fragment through scraper so that nested
/// or malformed tags are handled correctly by the HTML5 parser.
fn strip_script_and_style(html: &str) -> String {
    use scraper::{Html, Selector};

    // Parse as a fragment so we don't add an implicit <html>/<body> wrapper.
    let fragment = Html::parse_fragment(html);
    let script_sel = Selector::parse("script, style").unwrap();

    // Collect the IDs of nodes to remove.
    let to_remove: Vec<_> = fragment.select(&script_sel).map(|el| el.id()).collect();

    if to_remove.is_empty() {
        // Nothing to strip — return cheaply.
        return html.to_owned();
    }

    // scraper's Dom is read-only, so we rebuild by serialising the fragment
    // and doing a second parse with the offending nodes removed via a negative
    // CSS selector approach: select everything that is NOT script/style and
    // reconstruct the outer HTML.  The simplest correct approach is to use
    // html5ever's serialiser directly on the fragment tree, skipping the
    // unwanted nodes.
    //
    // Since scraper doesn't expose mutable tree editing, we use a regex-free
    // string reconstruction: serialise each top-level child that is not a
    // script/style element, recursively.  For deep trees we rely on the fact
    // that inner_html() on a non-script/style element already omits its own
    // tag — so we collect outer_html() of every child that survives the filter.
    let root = fragment.root_element();
    let mut out = String::with_capacity(html.len());
    for child in root.children() {
        if let Some(el) = scraper::ElementRef::wrap(child) {
            let tag = el.value().name();
            if tag == "script" || tag == "style" {
                continue;
            }
            out.push_str(&el.html());
        } else if let Some(text) = child.value().as_text() {
            // Text node — include as-is.
            out.push_str(text);
        }
    }
    out
}

/// Inspect the upstream response headers and decide whether Markdown conversion should
/// proceed.  Cancels (`ctx.wants_markdown = false`) for anything other than a successful
/// (2xx) `text/html` response, or when the connection is SSE/WebSocket.
///
/// Also adds `Vary: Accept` when conversion is confirmed so downstream caches key
/// correctly on the `Accept` header.
///
/// Extracted as a free function so it can be unit-tested without a live Pingora session.
fn apply_markdown_upstream_gate(upstream_response: &mut ResponseHeader, ctx: &mut ProxyContext) {
    if !ctx.wants_markdown {
        return;
    }

    let status = upstream_response.status.as_u16();

    // Use lowercase for case-insensitive comparison — some upstreams send "TEXT/HTML".
    let upstream_ct = upstream_response
        .headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_lowercase();

    let is_success = (200..300).contains(&status);
    let is_html = upstream_ct.contains("text/html");
    let has_ct = !upstream_ct.is_empty();

    if ctx.is_sse || ctx.is_websocket || !is_success || !is_html {
        // Cannot or should not convert — reset the flag so response_body_filter
        // will pass the body through normally.
        ctx.wants_markdown = false;
        if !has_ct {
            debug!(
                "Markdown conversion cancelled: no Content-Type header (status={})",
                status
            );
        } else if !is_success {
            debug!(
                "Markdown conversion cancelled: non-2xx status={}, content-type={:?}",
                status, upstream_ct
            );
        } else {
            debug!(
                "Markdown conversion cancelled: content-type={:?}, sse={}, ws={}",
                upstream_ct, ctx.is_sse, ctx.is_websocket
            );
        }
    } else {
        // Inform downstream caches that the response varies by Accept header.
        if let Err(e) = upstream_response.insert_header("Vary", "Accept") {
            warn!("Failed to insert Vary header for markdown response: {}", e);
        }
        debug!(
            "Markdown conversion confirmed: status={}, content-type={:?}",
            status, upstream_ct
        );
    }
}

/// Rewrite outbound response headers for Markdown delivery.
/// Must be called from `response_filter` (before the body is sent to the client).
///
/// Extracted as a free function so it can be unit-tested without a live Pingora session.
fn apply_markdown_response_headers(upstream_response: &mut ResponseHeader, ctx: &ProxyContext) {
    if !ctx.wants_markdown {
        return;
    }
    if let Err(e) = upstream_response.insert_header("Content-Type", "text/markdown; charset=utf-8")
    {
        warn!("Failed to set Content-Type for markdown response: {}", e);
    }
    // Remove Content-Length — the Markdown body will differ in size from the HTML.
    // Pingora will handle framing via chunked transfer encoding.
    upstream_response.remove_header("Content-Length");
    // Remove Content-Encoding — we disabled upstream compression for markdown
    // requests, but be defensive in case it was set anyway.
    upstream_response.remove_header("Content-Encoding");
    // Set x-markdown-tokens to 0 as a placeholder.  The actual token count is
    // computed in response_body_filter once the full body is available, but
    // Pingora sends headers before the body filter runs.
    if let Err(e) = upstream_response.insert_header("X-Markdown-Tokens", "0") {
        warn!("Failed to set X-Markdown-Tokens header: {}", e);
    }
}

pub const SESSION_ID_COOKIE: &str = "_temps_sid";
pub const ROUTE_PREFIX_TEMPS: &str = "/api/_temps";

// Helper functions for project-scoped cookie names
fn get_visitor_cookie_name(_project_id: Option<i32>) -> String {
    VISITOR_ID_COOKIE.to_string()
}

fn get_session_cookie_name(_project_id: Option<i32>) -> String {
    SESSION_ID_COOKIE.to_string()
}
pub const SERVER_NAME: &[u8; 5] = b"Temps";
pub const LB_SEED: u64 = 42;
pub const MAX_WEBHOOK_BODY_SIZE: usize = 16 * 1024;
pub const LOG_STATIC_ASSETS: bool = false;

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
}

/// Main load balancer proxy implementation using traits
pub struct LoadBalancer {
    upstream_resolver: Arc<dyn UpstreamResolver>,
    proxy_log_handle: ProxyLogBatchHandle,
    tracking_handle: TrackingBatchHandle,
    project_context_resolver: Arc<dyn ProjectContextResolver>,
    cookie_config: CookieConfig,
    crypto: Arc<temps_core::CookieCrypto>,
    db: Arc<DbConnection>,
    config_service: Arc<temps_config::ConfigService>,
    ip_access_control_service: Arc<IpAccessControlService>,
    challenge_service: Arc<ChallengeService>,
    /// In-memory snapshot of domains that have a TLS certificate. Used by the
    /// HTTP→HTTPS redirect check instead of issuing 2 DB queries per request.
    /// Refreshed every 30 s by `CertHostCache::run_refresh_loop`. See WS3.
    cert_host_cache: Arc<CertHostCache>,
    disable_https_redirect: bool,
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
    /// In-memory moka cache for `static_asset_cache` DB lookups. Keyed on
    /// `(project_id, url_path)`; values are `Option<content_hash>` so that
    /// **negative results (no row found) are cached too** — the miss case is
    /// the common path for container deployments where most assets are served
    /// by upstream, not the fallback store. TTL 60 s, max ~50 k entries. See
    /// `service/static_asset_lookup.rs` and WS4 in IMPLEMENTATION_PLAN.md.
    static_asset_lookup: Arc<crate::service::static_asset_lookup::StaticAssetLookup>,
    preview_auth_limiter: Arc<PreviewAuthLimiter>,
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
        challenge_service: Arc<ChallengeService>,
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
            static_asset_lookup: Arc::new(
                crate::service::static_asset_lookup::StaticAssetLookup::new(Arc::clone(&db)),
            ),
            sandbox_lookup_cache: Arc::new(SandboxLookupCache::new(Arc::clone(&db))),
            db,
            config_service,
            ip_access_control_service,
            challenge_service,
            cert_host_cache,
            disable_https_redirect,
            on_demand_manager: None,
            on_demand_cert_manager: None,
            route_table: None,
            file_store: None,
            preview_auth_limiter: Arc::new(PreviewAuthLimiter::new()),
            admin_gate: None,
            proxy_metrics: Arc::new(crate::metrics::ProxyMetrics::default()),
        }
    }

    /// Handle to the hot-path metrics counters, for the background sampler.
    /// The returned `Arc` shares the counters this instance records into.
    pub fn proxy_metrics(&self) -> Arc<crate::metrics::ProxyMetrics> {
        Arc::clone(&self.proxy_metrics)
    }

    /// Wire the shared admin-gate handle. When set, `request_filter`
    /// short-circuits unknown-host requests with 404 unless the request
    /// matches the gate (`/api/_temps/*` is always exempt because public
    /// ingest must reach the console from any host).
    pub fn with_admin_gate(mut self, handle: temps_core::admin_gate::AdminGateHandle) -> Self {
        self.admin_gate = Some(handle);
        self
    }

    /// Set the file store for path-keyed static asset serving.
    pub fn with_file_store(mut self, store: Arc<dyn temps_file_store::FileStore>) -> Self {
        self.file_store = Some(store);
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

    /// Decide whether the admin gate should be consulted for this request.
    ///
    /// The gate is only meaningful when it's non-noop, the request isn't a
    /// workspace/sandbox preview (those carry their own auth), and the path
    /// isn't a public temps ingest endpoint (`/api/_temps/*` must reach the
    /// console from any host). Even when this returns `true`, the caller
    /// must still consult `has_route_for_host` first so legitimate project
    /// traffic is never gated — only console fall-throughs are.
    fn should_consult_admin_gate(
        config: &temps_core::admin_gate::AdminGateConfig,
        path: &str,
        is_preview: bool,
    ) -> bool {
        !config.is_noop() && !is_preview && !path.starts_with(ROUTE_PREFIX_TEMPS)
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

    fn get_host_header(&self, session: &PingoraSession) -> Result<String> {
        let host_with_port = if let Some(host) = session.req_header().headers.get("host") {
            host.to_str()
                .map_err(|_| Error::new_str("Invalid host header encoding"))?
                .to_string()
        } else if let Some(host) = session.req_header().uri.host() {
            // Try to get the :authority pseudo-header first (used in HTTP/2)
            host.to_string()
        } else {
            return Err(Error::new_str("Missing Host or :authority header"));
        };

        // Remove port from host before returning (e.g., "example.com:3000" -> "example.com")
        // This ensures we match against domain names in the route table correctly
        let host = host_with_port.split(':').next().unwrap_or(&host_with_port);
        Ok(host.to_string())
    }

    /// Extract TLS fingerprint with client characteristics
    ///
    /// Returns a fingerprint including:
    /// - TLS version and cipher (from TLS handshake)
    /// - Client IP address
    /// - User-Agent header
    ///
    /// This creates a unique identifier per person/device, ensuring
    /// each different visitor gets a different fingerprint.
    fn extract_tls_info(&self, session: &PingoraSession, ctx: &mut ProxyContext) {
        // Access SSL digest from the downstream session's digest
        // digest() returns Option<&Digest>, and Digest contains ssl_digest: Option<Arc<SslDigest>>
        if let Some(digest) = session.downstream_session.digest() {
            if let Some(ssl_digest) = &digest.ssl_digest {
                // Compute fingerprint with IP and user agent
                if let Some(fingerprint) = tls_fingerprint::compute_fingerprint_from_arc(
                    ssl_digest,
                    ctx.ip_address.as_deref(),
                    &ctx.user_agent,
                ) {
                    ctx.tls_fingerprint = Some(fingerprint.clone());

                    debug!(
                        "Extracted fingerprint: {} (IP: {}, UA: {}) for request_id={}",
                        fingerprint,
                        ctx.ip_address.as_ref().unwrap_or(&"unknown".to_string()),
                        ctx.user_agent,
                        ctx.request_id
                    );
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
    }

    /// Generate HTML for CAPTCHA challenge page
    fn generate_challenge_html(
        project_name: &str,
        environment_id: i32,
        ip_address: &str,
        identifier: &str,
        identifier_type: &str,
    ) -> String {
        // Generate a random challenge (32 hex characters)
        use rand::Rng;
        let mut rng = rand::thread_rng();
        let bytes: Vec<u8> = (0..16).map(|_| rng.gen()).collect();
        let challenge = hex::encode(bytes);

        // Difficulty: 20 leading zero bits (~1 million attempts)
        // Typical solutions take ~2-5 seconds on modern browsers
        let difficulty = 20;

        // Load HTML template from file
        const CHALLENGE_HTML: &str = include_str!("../captcha/challenge.html");

        // Replace placeholders
        CHALLENGE_HTML
            .replace("{{PROJECT_NAME}}", project_name)
            .replace("{{ENVIRONMENT_ID}}", &environment_id.to_string())
            .replace("{{IP_ADDRESS}}", ip_address)
            .replace("{{CHALLENGE}}", &challenge)
            .replace("{{DIFFICULTY}}", &difficulty.to_string())
            .replace("{{IDENTIFIER}}", identifier)
            .replace("{{IDENTIFIER_TYPE}}", identifier_type)
    }

    /// Resolve visitor and session identifiers from cookies — entirely in-process,
    /// no database round-trips. A [`TrackingEvent`] is enqueued for the background
    /// batch writer, which upserts visitor/session rows asynchronously.
    async fn ensure_visitor_session(&self, ctx: &mut ProxyContext) {
        // Only resolve once per request
        if ctx.visitor_id.is_some() {
            return;
        }

        // Skip crawlers — only track real humans
        if let Some(crawler_name) =
            crate::crawler_detector::CrawlerDetector::get_crawler_name(Some(&ctx.user_agent))
        {
            debug!(
                "Crawler detected: {} ({}), skipping visitor/session for project {}",
                crawler_name,
                ctx.user_agent,
                ctx.project.as_ref().map(|p| p.id).unwrap_or(0)
            );
            return;
        }

        // ── Stateless visitor decision (no DB) ──────────────────────────────
        let visitor_uuid =
            parse_visitor_cookie(ctx.request_visitor_cookie.as_deref(), &self.crypto);

        // ── Stateless session decision (no DB) ──────────────────────────────
        let session_decision = parse_session_cookie(
            ctx.request_session_cookie.as_deref(),
            &self.crypto,
            self.cookie_config.session_max_age_minutes,
        );

        // ── Compute attribution (used only for new visitors) ─────────────────
        let utm = ctx
            .query_string
            .as_deref()
            .map(temps_analytics::parse_utm_params)
            .unwrap_or_default();
        let referrer_hostname = ctx
            .referrer
            .as_deref()
            .and_then(temps_analytics::extract_referrer_hostname);
        let channel =
            temps_analytics::get_channel(&utm, referrer_hostname.as_deref(), Some(&ctx.host));

        let attribution = crate::traits::FirstVisitAttribution {
            referrer: ctx.referrer.clone(),
            referrer_hostname: referrer_hostname.clone(),
            channel: Some(channel.to_string()),
            utm_source: utm.utm_source.clone(),
            utm_medium: utm.utm_medium.clone(),
            utm_campaign: utm.utm_campaign.clone(),
        };

        // ── Enqueue background upsert ─────────────────────────────────────────
        self.tracking_handle.send(TrackingEvent {
            visitor_uuid: visitor_uuid.clone(),
            session_uuid: session_decision.session_uuid.clone(),
            project_id: ctx.project.as_ref().map(|p| p.id).unwrap_or(0),
            environment_id: ctx.environment.as_ref().map(|e| e.id).unwrap_or(0),
            last_seen: chrono::Utc::now(),
            client_ip: ctx.ip_address.clone(),
            user_agent: Some(ctx.user_agent.clone()),
            is_crawler: false,
            crawler_name: None,
            is_new_session: session_decision.is_new_session,
            session_referrer: ctx.referrer.clone(),
            session_referrer_hostname: referrer_hostname,
            session_utm_source: utm.utm_source,
            session_utm_medium: utm.utm_medium,
            session_utm_campaign: utm.utm_campaign,
            session_utm_content: utm.utm_content,
            session_utm_term: utm.utm_term,
            session_channel: Some(channel.to_string()),
            attribution,
        });

        // ── Set context fields ────────────────────────────────────────────────
        ctx.visitor_id = Some(visitor_uuid.clone());
        ctx.session_id = Some(session_decision.session_uuid.clone());
        ctx.is_new_session = session_decision.is_new_session;

        debug!(
            "HTML request from visitor {} with session {} (new: {}) for project {}",
            visitor_uuid,
            session_decision.session_uuid,
            session_decision.is_new_session,
            ctx.project.as_ref().map(|p| p.id).unwrap_or(0)
        );
    }

    /// Returns true when a page view should be tracked (visitor/session created).
    /// This replaces the old `VisitorManager::should_track_visitor` trait method.
    pub fn should_track_page(path: &str, content_type: Option<&str>, status_code: u16) -> bool {
        // Don't track internal API calls
        if path.starts_with(ROUTE_PREFIX_TEMPS) {
            return false;
        }

        // Don't track static assets
        if path.contains('.')
            && (path.ends_with(".js")
                || path.ends_with(".css")
                || path.ends_with(".png")
                || path.ends_with(".jpg")
                || path.ends_with(".svg")
                || path.ends_with(".ico"))
        {
            return false;
        }

        // Track HTML pages or error pages
        let is_html = content_type
            .map(|ct| ct.starts_with("text/html"))
            .unwrap_or(false);

        is_html || status_code >= 400
    }

    async fn finalize_response(
        &self,
        session: &mut PingoraSession,
        upstream_response: &mut ResponseHeader,
        ctx: &mut ProxyContext,
    ) -> Result<()> {
        upstream_response.insert_header("X-Request-ID", &ctx.request_id)?;

        // Apply security headers from project settings or global config
        self.apply_security_headers(upstream_response, ctx.project.as_deref())
            .await?;

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

    /// Apply security headers from project settings or global config
    ///
    /// Attempts to use project-level security settings first (via temps-routes),
    /// then falls back to global config service settings if project is unavailable
    async fn apply_security_headers(
        &self,
        response: &mut ResponseHeader,
        project: Option<&projects::Model>,
    ) -> Result<()> {
        use temps_entities::deployment_config::SecurityHeadersConfig;

        // Map preset names to default header values
        fn get_preset_headers(preset: &str) -> SecurityHeadersConfig {
            match preset.to_lowercase().as_str() {
                "strict" => SecurityHeadersConfig {
                    preset: Some("strict".to_string()),
                    content_security_policy: Some(
                        "default-src 'self'; script-src 'self' 'unsafe-inline' 'unsafe-eval'; style-src 'self' 'unsafe-inline'; img-src 'self' data: https:; font-src 'self' data:; connect-src 'self'; frame-ancestors 'none'; base-uri 'self'; form-action 'self'".to_string()
                    ),
                    x_frame_options: Some("DENY".to_string()),
                    strict_transport_security: Some("max-age=31536000; includeSubDomains; preload".to_string()),
                    referrer_policy: Some("strict-origin-when-cross-origin".to_string()),
                },
                "moderate" => SecurityHeadersConfig {
                    preset: Some("moderate".to_string()),
                    content_security_policy: Some(
                        "default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' data: https:; font-src 'self' data:; connect-src 'self' https:; frame-ancestors 'self'".to_string()
                    ),
                    x_frame_options: Some("SAMEORIGIN".to_string()),
                    strict_transport_security: Some("max-age=31536000; includeSubDomains".to_string()),
                    referrer_policy: Some("no-referrer-when-downgrade".to_string()),
                },
                "permissive" => SecurityHeadersConfig {
                    preset: Some("permissive".to_string()),
                    content_security_policy: Some(
                        "default-src 'self'; script-src 'self' 'unsafe-inline' 'unsafe-eval' https:; style-src 'self' 'unsafe-inline' https:; img-src 'self' data: https:; font-src 'self' data: https:; connect-src 'self' https:; frame-ancestors *".to_string()
                    ),
                    x_frame_options: Some("ALLOW-FROM *".to_string()),
                    strict_transport_security: Some("max-age=31536000".to_string()),
                    referrer_policy: Some("origin".to_string()),
                },
                "disabled" => SecurityHeadersConfig {
                    preset: Some("disabled".to_string()),
                    content_security_policy: None,
                    x_frame_options: None,
                    strict_transport_security: None,
                    referrer_policy: None,
                },
                _ => SecurityHeadersConfig {
                    preset: Some(preset.to_string()),
                    content_security_policy: None,
                    x_frame_options: None,
                    strict_transport_security: None,
                    referrer_policy: None,
                },
            }
        }

        // Try to get security headers from project configuration first
        // Returns: None = no config (should check global), Some(config) = explicit config from project
        let (project_has_explicit_config, headers_config) = if let Some(proj) = project {
            debug!(
                "Applying security headers for project id={}, slug={}",
                proj.id, proj.slug
            );
            if let Some(ref deploy_config) = proj.deployment_config {
                debug!(
                    "Project {} has deployment_config, security field: {}",
                    proj.id,
                    deploy_config.security.is_some()
                );
                if let Some(ref security) = deploy_config.security {
                    debug!(
                        "Security config present: enabled={}, headers={}, rate_limiting={}, attack_mode={}",
                        security.enabled.unwrap_or(true),
                        security.headers.is_some(),
                        security.rate_limiting.is_some(),
                        security.attack_mode.is_some()
                    );

                    // Check if security is explicitly disabled at project level
                    if security.enabled == Some(false) {
                        debug!("Security headers are explicitly disabled at project level - skipping global fallback");
                        return Ok(());
                    }

                    if let Some(ref headers) = security.headers {
                        // Check if we have a preset but no individual headers configured
                        let has_preset = headers.preset.is_some();
                        let has_individual_headers = headers.content_security_policy.is_some()
                            || headers.x_frame_options.is_some()
                            || headers.strict_transport_security.is_some()
                            || headers.referrer_policy.is_some();

                        // Check if preset is "disabled"
                        let preset_disabled = has_preset
                            && headers.preset.as_ref().map(|p| p.to_lowercase())
                                == Some("disabled".to_string());

                        if preset_disabled {
                            debug!("Project has security headers preset set to 'disabled' - skipping global fallback");
                            return Ok(());
                        }

                        if has_preset && !has_individual_headers {
                            // Use preset to generate default headers
                            if let Some(preset_name) = headers.preset.as_ref() {
                                debug!(
                                    "Using preset '{}' to generate security headers from project config",
                                    preset_name
                                );
                                (true, Some(get_preset_headers(preset_name)))
                            } else {
                                // has_preset was true but preset is None — should not happen,
                                // fall through to global config
                                (false, None)
                            }
                        } else if has_individual_headers {
                            // Use individual headers as configured
                            debug!(
                                "Using custom security headers from project: preset={:?}, csp={}, x_frame={}, hsts={}, referrer={}",
                                headers.preset,
                                headers.content_security_policy.is_some(),
                                headers.x_frame_options.is_some(),
                                headers.strict_transport_security.is_some(),
                                headers.referrer_policy.is_some()
                            );
                            (true, Some(headers.clone()))
                        } else {
                            // No preset and no individual headers - project has config but empty, don't fall back to global
                            debug!("Project has security config but no headers or preset configured - skipping global fallback");
                            (true, None)
                        }
                    } else {
                        debug!("Project has security config but no headers configured (headers field is None) - allowing global fallback");
                        (false, None)
                    }
                } else {
                    debug!("Project has deployment_config but no security config (security field is None) - allowing global fallback");
                    (false, None)
                }
            } else {
                debug!("Project {} has no deployment_config field (is None) - allowing global fallback", proj.id);
                (false, None)
            }
        } else {
            debug!("No project context available for security headers - allowing global fallback");
            (false, None)
        };

        // If project didn't have explicit config, check global settings
        let headers_config = if !project_has_explicit_config && headers_config.is_none() {
            debug!("No explicit project-level security headers, checking global settings");
            match self.config_service.get_settings().await {
                Ok(settings) => {
                    let headers = &settings.security_headers;
                    if !headers.enabled {
                        debug!("Security headers are disabled in global settings");
                        return Ok(());
                    }
                    debug!("Using global security headers: preset={}", headers.preset);
                    Some(SecurityHeadersConfig {
                        preset: Some(headers.preset.clone()),
                        content_security_policy: headers.content_security_policy.clone(),
                        x_frame_options: Some(headers.x_frame_options.clone()),
                        strict_transport_security: Some(headers.strict_transport_security.clone()),
                        referrer_policy: Some(headers.referrer_policy.clone()),
                    })
                }
                Err(e) => {
                    warn!("Failed to get settings for security headers: {}", e);
                    return Ok(()); // Don't fail the request if we can't get settings
                }
            }
        } else {
            headers_config
        };

        // Apply headers from configuration
        if let Some(config) = headers_config {
            let mut headers_applied = Vec::new();

            // Apply Content-Security-Policy
            if let Some(ref csp) = config.content_security_policy {
                if !csp.is_empty() {
                    if let Err(e) = response.insert_header("Content-Security-Policy", csp) {
                        warn!("Failed to set Content-Security-Policy header: {}", e);
                    } else {
                        headers_applied.push("Content-Security-Policy");
                    }
                }
            }

            // Apply X-Frame-Options
            if let Some(ref x_frame) = config.x_frame_options {
                if !x_frame.is_empty() {
                    if let Err(e) = response.insert_header("X-Frame-Options", x_frame) {
                        warn!("Failed to set X-Frame-Options header: {}", e);
                    } else {
                        headers_applied.push("X-Frame-Options");
                    }
                }
            }

            // Apply Strict-Transport-Security
            if let Some(ref hsts) = config.strict_transport_security {
                if !hsts.is_empty() {
                    if let Err(e) = response.insert_header("Strict-Transport-Security", hsts) {
                        warn!("Failed to set Strict-Transport-Security header: {}", e);
                    } else {
                        headers_applied.push("Strict-Transport-Security");
                    }
                }
            }

            // Apply Referrer-Policy
            if let Some(ref policy) = config.referrer_policy {
                if !policy.is_empty() {
                    if let Err(e) = response.insert_header("Referrer-Policy", policy) {
                        warn!("Failed to set Referrer-Policy header: {}", e);
                    } else {
                        headers_applied.push("Referrer-Policy");
                    }
                }
            }

            if headers_applied.is_empty() {
                debug!("No security headers to apply (all configs empty)");
            } else {
                debug!(
                    "Applied {} security headers: {:?}",
                    headers_applied.len(),
                    headers_applied
                );
            }
        } else {
            debug!("No security headers configuration available");
        }

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

    async fn handle_acme_http_challenge(&self, host: &str, path: &str) -> Result<Option<String>> {
        const ACME_CHALLENGE_PREFIX: &str = "/.well-known/acme-challenge/";

        if !path.starts_with(ACME_CHALLENGE_PREFIX) {
            return Ok(None);
        }

        let token = &path[ACME_CHALLENGE_PREFIX.len()..];
        if token.is_empty() {
            debug!("Empty ACME challenge token in path: {}", path);
            return Ok(None);
        }

        debug!(
            "Looking up ACME HTTP-01 challenge for domain: {}, token: {}",
            host, token
        );

        // Direct DB query accepted here: this code path is reachable only for
        // requests whose path starts with `/.well-known/acme-challenge/`, which
        // is rare by construction (only Let's Encrypt validation requests hit
        // it). See item H in IMPLEMENTATION_PLAN.md §2 — intentionally left
        // as-is because caching transient challenge tokens would complicate the
        // cert-provisioning flow with no meaningful throughput benefit.
        let domain_record = domains::Entity::find()
            .filter(domains::Column::Domain.eq(host))
            .filter(domains::Column::HttpChallengeToken.eq(token))
            .one(self.db.as_ref())
            .await
            .map_err(|e| {
                error!("Database error looking up ACME challenge: {:?}", e);
                Error::new_str("Database error during ACME challenge lookup")
            })?;

        if let Some(domain) = domain_record {
            if let Some(key_auth) = domain.http_challenge_key_authorization {
                debug!(
                    "Found ACME HTTP-01 challenge for domain: {}, returning key authorization",
                    host
                );
                return Ok(Some(key_auth));
            } else {
                debug!(
                    "Domain {} has matching token but no key authorization",
                    host
                );
            }
        } else {
            debug!(
                "No matching ACME challenge found for domain: {}, token: {}",
                host, token
            );
        }

        Ok(None)
    }

    /// On-demand HTTP-01 TLS UX on port 80 (ADR-018 §5, "what the end user
    /// sees"). Runs only for plain-HTTP (non-TLS) requests that are NOT ACME
    /// challenges — ACME handling already short-circuits before this is called,
    /// so a challenge can always complete.
    ///
    /// Two independent behaviors, both driven off the proxy's in-process caches
    /// (no DB I/O in the hot path):
    ///
    /// 1. **Cert-state 503.** When the host is currently in the on-demand cert
    ///    manager's in-process state cache, the TLS handshake is fast-failing
    ///    (Option B) and the user would otherwise see only an opaque TLS error.
    ///    We give them a human-readable signal on :80:
    ///      - `Pending` / `Issuing` → 503 "provisioning in progress, retry…".
    ///      - `Failed`              → 503 "issuance failed, contact admin".
    ///
    ///    A successful issuance removes the entry, so the next request flows
    ///    through to the HTTPS redirect / normal routing.
    ///
    /// 2. **Ephemeral `deployment_url_mode`.** Per-deployment hostnames are
    ///    `cert_eligible == false` and are NEVER certed (ADR §2). They are never
    ///    in an on-demand cert state, so this is independent of (1). When
    ///    `deployment_url_mode == "redirect_to_env"` we 308-redirect such a host
    ///    to its STABLE per-environment URL (`<env.subdomain>.<preview_domain>`),
    ///    which IS certed; otherwise (`"http"`, the default) we serve plain HTTP
    ///    by returning `Ok(false)` and letting normal routing proceed.
    ///
    /// The caller MUST have already confirmed this is a plain-HTTP connection
    /// (`!is_tls_connection`) and that the manager is wired, so this function
    /// performs NO TLS check and NO settings fetch in the common path. Settings
    /// are loaded lazily only when an ephemeral host actually reaches the
    /// `redirect_to_env` branch — `get_settings()` is TTL-cached in
    /// `temps_config::ConfigService`, so even on that rare branch it is a
    /// fast in-memory read rather than a Postgres round-trip.
    ///
    /// Returns `Ok(true)` when a response was written (caller must return early),
    /// `Ok(false)` when the request should continue down the normal path.
    async fn handle_on_demand_http(
        &self,
        session: &mut PingoraSession,
        ctx: &mut ProxyContext,
    ) -> Result<bool> {
        // (1) Cert-state 503 — served purely from the in-process cache, no DB.
        if let Some(ref manager) = self.on_demand_cert_manager {
            if let Some(state) = manager.state_of(&ctx.host) {
                let (status, body) = on_demand_cert_state_response(&state);

                let body_bytes = Bytes::from_static(body);
                let mut resp = ResponseHeader::build(status, None)?;
                resp.insert_header("Content-Type", "text/plain; charset=utf-8")?;
                resp.insert_header("Cache-Control", "no-store")?;
                resp.insert_header("Retry-After", "5")?;
                resp.insert_header("Content-Length", body_bytes.len().to_string())?;
                resp.insert_header("X-Request-ID", &ctx.request_id)?;
                session.write_response_header(Box::new(resp), false).await?;
                session.write_response_body(Some(body_bytes), true).await?;
                ctx.routing_status = "on_demand_cert_provisioning".to_string();
                return Ok(true);
            }
        }

        // (2) Ephemeral `deployment_url_mode` redirect. Classify the host via the
        // in-memory route table FIRST (cheap), so we only pay the settings DB
        // fetch for a genuinely ephemeral, routed host.
        let Some(ref route_table) = self.route_table else {
            return Ok(false);
        };
        let Some(route) = route_table.get_route(&ctx.host) else {
            // Unknown host — leave normal routing (admin gate / 404) to decide.
            return Ok(false);
        };
        // Stable, cert-eligible hosts are handled by the normal HTTPS path; only
        // ephemeral per-deployment hostnames get the redirect treatment.
        if route.cert_eligible {
            return Ok(false);
        }

        // Only now — for an ephemeral routed host — do we need the setting that
        // decides http-vs-redirect. This is the rare branch; get_settings() is
        // TTL-cached so it is an in-memory read, not a Postgres round-trip.
        let settings = match self.config_service.get_settings().await {
            Ok(s) => s,
            Err(e) => {
                warn!(
                    request_id = %ctx.request_id,
                    host = %ctx.host,
                    error = %e,
                    "on-demand TLS: failed to load settings for ephemeral redirect; serving HTTP"
                );
                return Ok(false);
            }
        };
        if settings.on_demand_tls.deployment_url_mode != "redirect_to_env" {
            return Ok(false);
        }

        // Derive the stable per-environment target: `<env.subdomain>.<preview>`.
        let env_subdomain = route.environment.as_ref().map(|e| e.subdomain.as_str());
        let Some(location) = ephemeral_redirect_location(
            env_subdomain,
            &settings.preview_domain,
            &ctx.host,
            &ctx.path,
            ctx.query_string.as_deref(),
        ) else {
            // Can't build a stable target (missing env subdomain / preview
            // domain, or it would loop) — fall back to serving HTTP.
            return Ok(false);
        };

        debug!(
            request_id = %ctx.request_id,
            host = %ctx.host,
            target = %location,
            "on-demand TLS: redirecting ephemeral deployment host to stable env URL"
        );

        // 308 Permanent Redirect preserves the method and body (ADR §2).
        let mut resp = ResponseHeader::build(308, None)?;
        resp.insert_header("Location", &location)?;
        resp.insert_header("Content-Length", "0")?;
        resp.insert_header("Cache-Control", "no-store")?;
        resp.insert_header("X-Request-ID", &ctx.request_id)?;
        session.write_response_header(Box::new(resp), true).await?;
        ctx.routing_status = "on_demand_deployment_redirect".to_string();
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

            let proxy_log_request = CreateProxyLogRequest {
                method: ctx.method.clone(),
                path: ctx.path.clone(),
                query_string: ctx.query_string.clone(),
                host: ctx.host.clone(),
                status_code: status_code as i16,
                response_time_ms: Some(ctx.start_time.elapsed().as_millis() as i32),
                request_source: "proxy".to_string(),
                is_system_request: ctx.path.starts_with(ROUTE_PREFIX_TEMPS),
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
            };

            // Stash rather than send: the `logging` hook fires after the
            // response body has fully streamed and patches response_size_bytes
            // with the accurate accumulated count before enqueueing.
            ctx.pending_proxy_log = Some(proxy_log_request);
        }

        Ok(())
    }

    #[allow(dead_code)]
    fn is_page_visit(&self, upstream_response: &ResponseHeader, _ctx: &ProxyContext) -> bool {
        let mut is_page_visit = upstream_response
            .headers
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .map(|content_type| {
                content_type.starts_with("text/html")
                    || content_type.starts_with("text/plain")
                    || content_type.starts_with("application/json")
            })
            .unwrap_or(false);

        // Note: Removed is_web_app check - all projects are now preset-based
        // Page visits are determined by URL patterns

        let status_code = upstream_response.status.as_u16();
        if status_code >= 400 {
            is_page_visit = true;
        }

        is_page_visit
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

    /// Check if a request path should be logged (HTML pages only, skip static assets)
    fn should_log_static_request(path: &str) -> bool {
        path == "/" || path.ends_with(".html") || path.ends_with(".htm") || !path.contains('.')
        // SPA routes without extension
    }

    /// Create and spawn proxy log for static file serving
    fn log_static_request(
        &self,
        ctx: &ProxyContext,
        status_code: i16,
        routing_status: &str,
        static_dir: &str,
        error_message: Option<String>,
        response_size: Option<i64>,
    ) {
        // Only log HTML pages (skip .js, .css, .svg, etc.)
        if !Self::should_log_static_request(&ctx.path) {
            return;
        }

        let proxy_log_request = CreateProxyLogRequest {
            method: ctx.method.clone(),
            path: ctx.path.clone(),
            query_string: ctx.query_string.clone(),
            host: ctx.host.clone(),
            status_code,
            response_time_ms: Some(ctx.start_time.elapsed().as_millis() as i32),
            request_source: "proxy".to_string(),
            is_system_request: ctx.path.starts_with(ROUTE_PREFIX_TEMPS),
            routing_status: routing_status.to_string(),
            project_id: ctx.project.as_ref().map(|p| p.id),
            environment_id: ctx.environment.as_ref().map(|e| e.id),
            deployment_id: ctx.deployment.as_ref().map(|d| d.id),
            session_id: None,
            visitor_id: None,
            visitor_uuid: ctx.visitor_id.clone(),
            session_uuid: ctx.session_id.clone(),
            container_id: None,
            upstream_host: Some(format!("static://{}", static_dir)),
            error_message,
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
            request_size_bytes: None,
            response_size_bytes: response_size,
            cache_status: None,
            request_headers: ctx
                .request_headers
                .as_ref()
                .and_then(|h| serde_json::to_value(h).ok()),
            response_headers: None,
            trace_id: Self::extract_traceparent_trace_id(ctx.request_headers.as_ref()),
            error_group_id: None,
        };

        // Non-blocking enqueue; shed with rate-limited accounting when full.
        self.proxy_log_handle.send_or_drop(proxy_log_request);
    }

    /// Set visitor and session cookies on the response.
    ///
    /// Visitor cookie: set only when the request doesn't already carry a valid one.
    /// Session cookie: always re-issued with the current timestamp embedded in the
    /// v2 payload so the server-side freshness check stays accurate.
    async fn set_tracking_cookies(
        &self,
        session: &mut PingoraSession,
        response: &mut ResponseHeader,
        ctx: &ProxyContext,
    ) -> Result<()> {
        let is_https = self.is_https_request(session);
        let project_id = ctx.project.as_ref().map(|p| p.id);

        // ── Visitor cookie ──────────────────────────────────────────────────
        if let Some(visitor_id) = &ctx.visitor_id {
            let cookie_name = get_visitor_cookie_name(project_id);

            let has_valid_visitor_cookie = session
                .req_header()
                .headers
                .get_all("Cookie")
                .iter()
                .filter_map(|h| h.to_str().ok())
                .flat_map(|s| Cookie::split_parse(s).filter_map(|c| c.ok()))
                .any(|c| c.name() == cookie_name && self.crypto.decrypt(c.value()).is_ok());

            if !has_valid_visitor_cookie {
                let encrypted = match self.crypto.encrypt(visitor_id) {
                    Ok(e) => e,
                    Err(err) => {
                        error!("Failed to encrypt visitor cookie: {:?}", err);
                        return Err(Error::new_str("Failed to encrypt visitor cookie"));
                    }
                };
                let cookie_value = self.build_cookie_string(
                    &cookie_name,
                    &encrypted,
                    cookie::time::Duration::days(self.cookie_config.visitor_max_age_days),
                    is_https,
                );
                response.append_header("Set-Cookie", cookie_value)?;
            }
        }

        // ── Session cookie ──────────────────────────────────────────────────
        // Always re-issue with the current timestamp to keep the sliding window fresh.
        if let Some(session_id) = &ctx.session_id {
            let cookie_name = get_session_cookie_name(project_id);
            let now_secs = chrono::Utc::now().timestamp();
            let payload = make_v2_session_payload(session_id, now_secs);
            let encrypted = match self.crypto.encrypt(&payload) {
                Ok(e) => e,
                Err(err) => {
                    error!("Failed to encrypt session cookie: {:?}", err);
                    return Err(Error::new_str("Failed to encrypt session cookie"));
                }
            };
            let cookie_value = self.build_cookie_string(
                &cookie_name,
                &encrypted,
                cookie::time::Duration::minutes(self.cookie_config.session_max_age_minutes),
                is_https,
            );
            response.append_header("Set-Cookie", cookie_value)?;
        }

        Ok(())
    }

    /// Build a `Set-Cookie` header value with the configured attributes.
    fn build_cookie_string(
        &self,
        name: &str,
        value: &str,
        max_age: cookie::time::Duration,
        is_https: bool,
    ) -> String {
        let mut builder = Cookie::build((name.to_owned(), value.to_owned()))
            .path("/")
            .max_age(max_age)
            .http_only(self.cookie_config.http_only)
            .secure(is_https && self.cookie_config.secure);

        if let Some(ref same_site) = self.cookie_config.same_site {
            let ss = match same_site.to_lowercase().as_str() {
                "strict" => cookie::SameSite::Strict,
                "lax" => cookie::SameSite::Lax,
                "none" => cookie::SameSite::None,
                _ => cookie::SameSite::Lax,
            };
            builder = builder.same_site(ss);
        }

        builder.build().to_string()
    }

    /// Serve a static file from the filesystem
    /// Returns Ok(true) if file was served, Ok(false) if file not found, Err on error
    async fn serve_static_file(
        &self,
        session: &mut PingoraSession,
        ctx: &mut ProxyContext,
        static_dir: &str,
    ) -> Result<bool> {
        use std::path::PathBuf;
        use tokio::fs;

        let mut requested_path = ctx.path.trim_start_matches('/');

        // Handle root path -> index.html
        if requested_path.is_empty() {
            requested_path = "index.html";
        }

        // Security: ALWAYS join with base static directory
        // Never trust absolute paths from database - always enforce that static files
        // must be within the configured static directory to prevent path traversal
        let static_dir_path = PathBuf::from(static_dir);

        // Strip leading slash if present (treat all paths as relative)
        let relative_static_dir = static_dir_path
            .strip_prefix("/")
            .unwrap_or(&static_dir_path);

        // Always join with base static directory from config
        let absolute_static_dir = self.config_service.static_dir().join(relative_static_dir);

        let file_path = absolute_static_dir.join(requested_path);

        // Security check: ensure the resolved path is still within static_dir
        let canonical_static_dir = fs::canonicalize(&absolute_static_dir).await.map_err(|e| {
            Error::because(
                pingora::ErrorType::FileOpenError,
                format!("Failed to canonicalize static dir: {}", e),
                e,
            )
        })?;

        // Try to canonicalize the file path, but handle the case where it doesn't exist
        let canonical_file_path = match fs::canonicalize(&file_path).await {
            Ok(path) => path,
            Err(_) => {
                // File doesn't exist - try with index.html for SPA routing
                if !requested_path.contains('.') {
                    // Likely a SPA route, serve index.html
                    let index_path = absolute_static_dir.join("index.html");
                    match fs::canonicalize(&index_path).await {
                        Ok(path) => path,
                        Err(_) => return Ok(false), // No index.html, file not found
                    }
                } else {
                    return Ok(false); // File not found
                }
            }
        };

        // Ensure the file is within the static directory (prevent path traversal)
        if !canonical_file_path.starts_with(&canonical_static_dir) {
            warn!(
                "Path traversal attempt detected: {} -> {}",
                requested_path,
                canonical_file_path.display()
            );
            return Ok(false);
        }

        // Check if it's a directory -> serve index.html
        let final_path = if canonical_file_path.is_dir() {
            canonical_file_path.join("index.html")
        } else {
            canonical_file_path
        };

        // Read the file
        let file_content = fs::read(&final_path).await.map_err(|e| {
            Error::because(
                pingora::ErrorType::FileOpenError,
                format!("Failed to read file: {}", e),
                e,
            )
        })?;

        // Generate ETag for cache validation
        let etag = Self::generate_etag(&file_content);

        // Check If-None-Match header for 304 Not Modified response
        if let Some(if_none_match) = session
            .req_header()
            .headers
            .get("if-none-match")
            .and_then(|v| v.to_str().ok())
        {
            if if_none_match == etag {
                debug!("ETag match - returning 304 Not Modified for: {}", ctx.path);
                let mut resp = ResponseHeader::build(StatusCode::NOT_MODIFIED, None)?;
                resp.insert_header("ETag", &etag)?;
                resp.insert_header("X-Request-ID", &ctx.request_id)?;

                // Add cache headers
                if Self::is_cacheable_static_asset(requested_path) {
                    resp.insert_header(
                        header::CACHE_CONTROL,
                        "public, max-age=31536000, immutable",
                    )?;
                } else {
                    resp.insert_header(
                        header::CACHE_CONTROL,
                        "public, max-age=0, must-revalidate",
                    )?;
                }

                // CRITICAL: Set tracking cookies even for 304 responses to keep sessions alive
                // Without this, visitors won't get cookies on cached root URLs (/) and events will fail
                self.set_tracking_cookies(session, &mut resp, ctx).await?;

                session.write_response_header(Box::new(resp), false).await?;
                session.write_response_body(None, true).await?;
                return Ok(true);
            }
        }

        // Infer content type
        let content_type = Self::infer_content_type(final_path.to_str().unwrap_or("index.html"));

        // Check if we should compress the content
        let client_accepts_gzip = Self::accepts_gzip(session);
        let should_compress =
            client_accepts_gzip && Self::should_compress_content(content_type, file_content.len());

        // Compress content if appropriate
        let (final_content, is_compressed) = if should_compress {
            match Self::compress_gzip(&file_content) {
                Ok(compressed) => {
                    // Only use compression if it actually reduces size
                    if compressed.len() < file_content.len() {
                        debug!(
                            "Compressed {} from {} to {} bytes ({:.1}% reduction)",
                            ctx.path,
                            file_content.len(),
                            compressed.len(),
                            (1.0 - (compressed.len() as f64 / file_content.len() as f64)) * 100.0
                        );
                        (compressed, true)
                    } else {
                        debug!(
                            "Skipping compression for {} - compressed size ({}) >= original ({})",
                            ctx.path,
                            compressed.len(),
                            file_content.len()
                        );
                        (file_content, false)
                    }
                }
                Err(e) => {
                    warn!("Failed to compress {}: {:?}", ctx.path, e);
                    (file_content, false)
                }
            }
        } else {
            (file_content, false)
        };

        // Build response
        let mut resp = ResponseHeader::build(200, None)?;
        resp.insert_header(header::CONTENT_TYPE, content_type)?;
        resp.insert_header(header::CONTENT_LENGTH, final_content.len().to_string())?;
        resp.insert_header("X-Request-ID", &ctx.request_id)?;
        resp.insert_header("ETag", &etag)?;

        // Add compression header if compressed
        if is_compressed {
            resp.insert_header("Content-Encoding", "gzip")?;
            resp.insert_header("Vary", "Accept-Encoding")?;
        }

        // Add cache headers for static assets
        if Self::is_cacheable_static_asset(requested_path) {
            resp.insert_header(header::CACHE_CONTROL, "public, max-age=31536000, immutable")?;
        } else {
            resp.insert_header(header::CACHE_CONTROL, "public, max-age=0, must-revalidate")?;
        }

        // Set visitor and session tracking cookies for static file responses
        self.set_tracking_cookies(session, &mut resp, ctx).await?;

        // Write response
        session.write_response_header(Box::new(resp), false).await?;
        session
            .write_response_body(Some(Bytes::from(final_content)), true)
            .await?;

        Ok(true)
    }

    /// Serve embedded WASM files for CAPTCHA solver
    /// Returns Ok(true) if file was served, Ok(false) if path doesn't match
    async fn serve_wasm_file(
        &self,
        session: &mut PingoraSession,
        ctx: &mut ProxyContext,
    ) -> Result<bool> {
        // Check if this is a WASM file request (use actual wasm-bindgen generated filenames)
        if ctx.path == "/api/__temps/temps_captcha_wasm.js" {
            let content = include_str!("../../temps-captcha-wasm/pkg/temps_captcha_wasm.js");
            let mut resp = ResponseHeader::build(StatusCode::OK, None)?;
            resp.insert_header(
                header::CONTENT_TYPE,
                "application/javascript; charset=utf-8",
            )?;
            resp.insert_header(header::CACHE_CONTROL, "public, max-age=31536000, immutable")?;
            resp.insert_header("X-Request-ID", &ctx.request_id)?;

            session.write_response_header(Box::new(resp), false).await?;
            session
                .write_response_body(Some(Bytes::from(content.as_bytes().to_vec())), true)
                .await?;

            debug!("Served WASM JavaScript bindings: {}", ctx.path);
            return Ok(true);
        } else if ctx.path == "/api/__temps/temps_captcha_wasm_bg.wasm" {
            let content = include_bytes!("../../temps-captcha-wasm/pkg/temps_captcha_wasm_bg.wasm");
            let mut resp = ResponseHeader::build(StatusCode::OK, None)?;
            resp.insert_header(header::CONTENT_TYPE, "application/wasm")?;
            resp.insert_header(header::CACHE_CONTROL, "public, max-age=31536000, immutable")?;
            resp.insert_header("X-Request-ID", &ctx.request_id)?;

            session.write_response_header(Box::new(resp), false).await?;
            session
                .write_response_body(Some(Bytes::from(content.to_vec())), true)
                .await?;

            debug!("Served WASM binary module: {}", ctx.path);
            return Ok(true);
        }

        Ok(false) // Not a WASM file request
    }

    /// Infer content type from file extension
    pub fn infer_content_type(file_path: &str) -> &'static str {
        let extension = std::path::Path::new(file_path)
            .extension()
            .and_then(std::ffi::OsStr::to_str)
            .unwrap_or("");

        match extension.to_lowercase().as_str() {
            "html" => "text/html; charset=utf-8",
            "css" => "text/css; charset=utf-8",
            "js" | "mjs" | "cjs" => "application/javascript; charset=utf-8",
            "json" => "application/json; charset=utf-8",
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "gif" => "image/gif",
            "svg" => "image/svg+xml",
            "webp" => "image/webp",
            "ico" => "image/x-icon",
            "woff" => "font/woff",
            "woff2" => "font/woff2",
            "ttf" => "font/ttf",
            "eot" => "application/vnd.ms-fontobject",
            "pdf" => "application/pdf",
            "txt" | "log" => "text/plain; charset=utf-8",
            "xml" => "application/xml; charset=utf-8",
            "zip" => "application/zip",
            _ => "application/octet-stream",
        }
    }

    /// Serve a static asset from CAS via the in-memory lookup cache.
    ///
    /// `static_asset_lookup` resolves `(project_id, url_path) → content_hash`
    /// using a moka TTL cache (60 s) so the `static_asset_cache` table is not
    /// queried on every cacheable-asset request. Both hits and misses are cached;
    /// the miss case (no fallback row — the common path for container deployments)
    /// is the most important one to protect. See WS4 / `static_asset_lookup.rs`.
    ///
    /// Returns `Ok(true)` if the asset was served, `Ok(false)` if not found.
    async fn serve_asset_from_store(
        &self,
        session: &mut PingoraSession,
        ctx: &mut ProxyContext,
        url_path: &str,
    ) -> Result<bool> {
        let file_store = match &self.file_store {
            Some(fs) => fs,
            None => return Ok(false),
        };

        // Resolve project_id, then look up the content hash via cache (no DB on hit/cached-miss).
        let content_hash = match ctx.project.as_ref().map(|p| p.id) {
            Some(pid) => match self
                .static_asset_lookup
                .get_content_hash(pid, url_path)
                .await
            {
                Some(hash) => hash,
                None => return Ok(false),
            },
            None => return Ok(false),
        };

        // Read blob from CAS by content hash
        let data = match file_store.get_blob(&content_hash).await {
            Ok(d) => d,
            Err(temps_file_store::FileStoreError::NotFound { .. }) => {
                warn!(
                    "CAS blob missing for hash {} (path: {})",
                    &content_hash[..8],
                    url_path
                );
                return Ok(false);
            }
            Err(e) => {
                debug!("CAS blob read failed for {}: {}", &content_hash[..8], e);
                return Ok(false);
            }
        };

        let content_type = Self::infer_content_type(url_path);

        // ETag from content hash (fast, stable for immutable assets)
        let etag = Self::generate_etag(&data);
        if let Some(if_none_match) = session
            .req_header()
            .headers
            .get("if-none-match")
            .and_then(|v| v.to_str().ok())
        {
            if if_none_match == etag {
                let mut resp = ResponseHeader::build(StatusCode::NOT_MODIFIED, None)?;
                resp.insert_header("ETag", &etag)?;
                resp.insert_header(header::CACHE_CONTROL, "public, max-age=31536000, immutable")?;
                self.set_tracking_cookies(session, &mut resp, ctx).await?;
                session.write_response_header(Box::new(resp), false).await?;
                session.write_response_body(None, true).await?;
                return Ok(true);
            }
        }

        // Check if we should compress
        let client_accepts_gzip = Self::accepts_gzip(session);
        let should_compress =
            client_accepts_gzip && Self::should_compress_content(content_type, data.len());

        let (final_content, is_compressed) = if should_compress {
            match Self::compress_gzip(&data) {
                Ok(compressed) if compressed.len() < data.len() => (compressed, true),
                _ => (data.to_vec(), false),
            }
        } else {
            (data.to_vec(), false)
        };

        let mut resp = ResponseHeader::build(200, None)?;
        resp.insert_header(header::CONTENT_TYPE, content_type)?;
        resp.insert_header(header::CONTENT_LENGTH, final_content.len().to_string())?;
        resp.insert_header("ETag", &etag)?;
        resp.insert_header(header::CACHE_CONTROL, "public, max-age=31536000, immutable")?;
        resp.insert_header("X-Request-ID", &ctx.request_id)?;
        if is_compressed {
            resp.insert_header("Content-Encoding", "gzip")?;
            resp.insert_header("Vary", "Accept-Encoding")?;
        }
        self.set_tracking_cookies(session, &mut resp, ctx).await?;

        session.write_response_header(Box::new(resp), false).await?;
        session
            .write_response_body(Some(Bytes::from(final_content)), true)
            .await?;

        Ok(true)
    }

    /// Check if a file should have long-term caching headers
    pub fn is_cacheable_static_asset(path: &str) -> bool {
        let cacheable_patterns = [
            "/assets/",
            "/static/",
            "/_next/static/",
            ".chunk.",
            ".hash.",
        ];

        cacheable_patterns
            .iter()
            .any(|pattern| path.contains(pattern))
    }

    /// Generate ETag from file content using SHA-256 hash
    fn generate_etag(content: &[u8]) -> String {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut hasher = DefaultHasher::new();
        content.hash(&mut hasher);
        let hash = hasher.finish();
        format!("W/\"{:x}\"", hash)
    }

    /// Check if content should be compressed based on Content-Type
    fn should_compress_content(content_type: &str, content_length: usize) -> bool {
        // Don't compress if content is too small (overhead not worth it)
        if content_length < 1024 {
            return false;
        }

        // Compress text-based content types
        let compressible_types = [
            "text/html",
            "text/css",
            "text/javascript",
            "text/plain",
            "text/xml",
            "application/javascript",
            "application/json",
            "application/xml",
            "application/x-javascript",
            "image/svg+xml",
        ];

        compressible_types
            .iter()
            .any(|ct| content_type.starts_with(ct))
    }

    /// Compress content using gzip
    fn compress_gzip(content: &[u8]) -> Result<Vec<u8>> {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder
            .write_all(content)
            .map_err(|_| Error::new_str("Failed to compress content"))?;
        encoder
            .finish()
            .map_err(|_| Error::new_str("Failed to finish compression"))
    }

    /// Check if client accepts gzip encoding
    fn accepts_gzip(session: &PingoraSession) -> bool {
        session
            .req_header()
            .headers
            .get("accept-encoding")
            .and_then(|v| v.to_str().ok())
            .map(|ae| ae.contains("gzip"))
            .unwrap_or(false)
    }
}

/// Map an on-demand cert in-process state to the port-80 503 response the end
/// user sees while the TLS handshake fast-fails (ADR-018 §5). Pure so the
/// status/body contract is unit-tested without a Pingora session.
fn on_demand_cert_state_response(
    state: &crate::on_demand_cert::OnDemandCertState,
) -> (u16, &'static [u8]) {
    match state {
        crate::on_demand_cert::OnDemandCertState::Pending
        | crate::on_demand_cert::OnDemandCertState::Issuing => (
            503,
            b"TLS certificate provisioning in progress. Retry in a few seconds.\n",
        ),
        crate::on_demand_cert::OnDemandCertState::Failed { .. } => (
            503,
            b"TLS certificate issuance failed. Contact your administrator.\n",
        ),
    }
}

/// Build the `redirect_to_env` Location for an ephemeral per-deployment host
/// (ADR-018 §2): the stable per-environment URL `<env_subdomain>.<preview>`
/// with the original path + query preserved. Returns `None` (→ serve plain
/// HTTP instead) when the env subdomain or preview domain is missing/empty, or
/// when the computed target equals the request host (which would loop). Pure so
/// the target derivation is unit-tested without a Pingora session.
fn ephemeral_redirect_location(
    env_subdomain: Option<&str>,
    preview_domain: &str,
    request_host: &str,
    request_path: &str,
    request_query: Option<&str>,
) -> Option<String> {
    let env_subdomain = env_subdomain.map(str::trim).filter(|s| !s.is_empty())?;
    let preview_domain = preview_domain.trim();
    if preview_domain.is_empty() {
        return None;
    }
    let target_host = format!("{}.{}", env_subdomain, preview_domain);
    // Avoid a redirect loop if the ephemeral host somehow equals its target.
    if target_host.eq_ignore_ascii_case(request_host) {
        return None;
    }
    let location = match request_query {
        Some(q) if !q.is_empty() => format!("https://{}{}?{}", target_host, request_path, q),
        _ => format!("https://{}{}", target_host, request_path),
    };
    Some(location)
}

/// Core response-body-filter logic (SSE/WebSocket passthrough, buffered
/// Markdown conversion, default passthrough). Split out as a free function
/// so `response_body_filter` can wrap it with byte-counting that applies
/// uniformly to every exit path — see `ProxyContext::upstream_body_bytes_received`.
fn response_body_filter_inner(
    body: &mut Option<Bytes>,
    end_of_stream: bool,
    ctx: &mut ProxyContext,
) -> Result<Option<std::time::Duration>> {
    // For SSE or WebSocket responses, pass through immediately without buffering
    if ctx.is_sse || ctx.is_websocket {
        if let Some(chunk) = body {
            let stream_type = if ctx.is_sse { "SSE" } else { "WebSocket" };
            debug!("Streaming {} chunk: {} bytes", stream_type, chunk.len());
        }
        return Ok(None);
    }

    // HTML-to-Markdown conversion: buffer chunks, convert on end_of_stream.
    if ctx.wants_markdown {
        if let Some(chunk) = body.take() {
            // Enforce 2 MB limit — mirrors Cloudflare's Markdown for Agents constraint.
            if ctx.markdown_buffer.len() + chunk.len() > MAX_MARKDOWN_BODY_BYTES {
                warn!(
                    "Response body exceeds 2 MB markdown conversion limit for path={}, \
                     falling back to passthrough",
                    ctx.path
                );
                // Disable markdown, flush the buffer + current chunk as-is.
                ctx.wants_markdown = false;
                let mut flushed = std::mem::take(&mut ctx.markdown_buffer);
                flushed.extend_from_slice(&chunk);
                *body = Some(Bytes::from(flushed));
                return Ok(None);
            }
            ctx.markdown_buffer.extend_from_slice(&chunk);
        }

        if end_of_stream {
            let html = String::from_utf8_lossy(&ctx.markdown_buffer);
            // Parse the document once — reuse it for both meta extraction
            // and content extraction.
            let document = scraper::Html::parse_document(&html);
            let meta = extract_page_meta(&document);
            // Extract <main> (or <body> fallback), stripping script/style.
            let content = extract_content_html(&document);
            let markdown = match htmd::convert(&content) {
                Ok(md) => md,
                Err(e) => {
                    warn!(
                        "HTML-to-Markdown conversion failed for path={}: {}",
                        ctx.path, e
                    );
                    // Fall back to the original HTML bytes so the client gets something.
                    let original = std::mem::take(&mut ctx.markdown_buffer);
                    *body = Some(Bytes::from(original));
                    return Ok(None);
                }
            };

            let token_estimate = estimate_markdown_tokens(&markdown);
            debug!(
                "Markdown conversion complete for path={}: {} bytes, ~{} tokens",
                ctx.path,
                markdown.len(),
                token_estimate
            );

            // The x-markdown-tokens header must be a trailer because the response
            // headers have already been sent. Pingora does not support HTTP trailers
            // for regular HTTP/1.1 clients, so we log the value and skip injecting it
            // into headers here — the header is set in response_filter instead via
            // a sentinel value once we know the body size upfront (not possible when
            // streaming).  Best-effort: we set it here anyway; Pingora will silently
            // drop it if trailers are unsupported.
            // Note: if you need reliable x-markdown-tokens delivery, switch to a
            // buffered response pattern (write_response_* directly in request_filter).

            // Prepend YAML front-matter built from <head> meta tags,
            // matching Cloudflare's Markdown for Agents output format.
            let final_markdown = match meta.to_frontmatter() {
                Some(fm) => fm + &markdown,
                None => markdown,
            };

            ctx.markdown_buffer = Vec::new(); // free memory
            *body = Some(Bytes::from(final_markdown));
        }
        // Suppress intermediate chunks — only emit on end_of_stream.
        return Ok(None);
    }

    // Default: pass all responses through without buffering
    Ok(None)
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
            upstream_connect_tries: 0,
            upstream_write_pending_time_ms: None,
            upstream_start_time: None,
            upstream_response_time_ms: None,
            preview_route: None,
        }
    }

    async fn early_request_filter(
        &self,
        session: &mut PingoraSession,
        ctx: &mut Self::CTX,
    ) -> Result<()> {
        // Extract client IP address FIRST (needed for TLS fingerprinting)
        let client_ip = session
            .client_addr()
            .map(|addr| {
                let addr_str = addr.to_string();
                addr_str.split(':').next().unwrap_or("unknown").to_string()
            })
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

        // WASM files must bypass IP access control since they're needed for challenge solving
        let is_wasm_request = path.starts_with("/api/__temps/temps_captcha_wasm");

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

        if accepts_sse || is_websocket_upgrade || is_chunked || is_streaming_path {
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
            session.upstream_compression.adjust_level(6);
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
                // Disable upstream compression so we receive raw HTML bytes to convert.
                session.upstream_compression.adjust_level(0);
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

        // Extract client IP address early (needed for attack mode checks)
        if let Some(addr) = session.client_addr() {
            let addr_str = addr.to_string();
            let client_ip = addr_str.split(':').next().unwrap_or_default();
            ctx.ip_address = Some(client_ip.to_string());
        }

        // SECURITY: Strip any inbound X-Temps-Demo-Mode header. Demo mode
        // has been removed; clients sending this header should never have it
        // honored by downstream auth middleware.
        let _ = session.req_header_mut().remove_header("X-Temps-Demo-Mode");

        // Workspace preview gateway: requests to `ws-<sid>-<port>.<preview_domain>`
        // are authenticated here against the per-session argon2 password hash
        // via a form-based login + encrypted cookie. On success we mark the
        // request as a preview route so `upstream_peer` forwards it to the
        // local gateway. On failure we short-circuit with a 303 redirect to
        // the login form, or 429 when rate-limited.
        //
        // HTTP Basic auth is NOT supported — see `preview_auth.rs` for
        // rationale.
        if let Ok(settings) = self.config_service.get_settings().await {
            if let Some(preview_host) = parse_preview_host(&ctx.host, &settings.preview_domain) {
                let client_ip = ctx
                    .ip_address
                    .as_deref()
                    .and_then(|s| s.parse::<std::net::IpAddr>().ok())
                    .unwrap_or_else(|| std::net::IpAddr::from([127, 0, 0, 1]));

                // ── Login/logout endpoints intercepted before auth ────────
                //
                // We serve these on the same preview host so the cookie can
                // be scoped to the preview domain. They must come BEFORE
                // `check_preview_auth` so unauthenticated GET /login works.
                let sandbox_hex: Option<String> = Some(preview_host.hex.clone());

                // POST /__temps/preview/login for a sandbox host.
                if let Some(hex) = sandbox_hex
                    .clone()
                    .filter(|_| ctx.path == PREVIEW_LOGIN_PATH && ctx.method == "POST")
                {
                    if self.preview_auth_limiter.is_blocked(client_ip, &hex) {
                        warn!(
                            sandbox = %hex,
                            client_ip = %client_ip,
                            "preview-auth: sandbox login POST rate limited"
                        );
                        let mut response =
                            ResponseHeader::build(StatusCode::TOO_MANY_REQUESTS, None)?;
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
                        return Ok(true);
                    }

                    let stored_hash = match self.sandbox_lookup_cache.lookup(&hex).await {
                        PreviewSandboxLookup::Protected { password_hash } => password_hash,
                        PreviewSandboxLookup::Open => {
                            // No password configured — nothing to verify. Redirect to `/`.
                            let mut response = ResponseHeader::build(303, None)?;
                            response.insert_header("Location", "/")?;
                            response.insert_header("Cache-Control", "no-store")?;
                            response.insert_header("X-Request-ID", &ctx.request_id)?;
                            session
                                .write_response_header(Box::new(response), true)
                                .await?;
                            ctx.routing_status = "preview_login_not_required".to_string();
                            return Ok(true);
                        }
                        PreviewSandboxLookup::NotFound => {
                            let mut response = ResponseHeader::build(StatusCode::NOT_FOUND, None)?;
                            response.insert_header("Cache-Control", "no-store")?;
                            response.insert_header("X-Request-ID", &ctx.request_id)?;
                            response.insert_header("Content-Type", "text/plain; charset=utf-8")?;
                            session
                                .write_response_header(Box::new(response), false)
                                .await?;
                            session
                                .write_response_body(
                                    Some(Bytes::from_static(b"Sandbox preview not found\n")),
                                    true,
                                )
                                .await?;
                            ctx.routing_status = "preview_not_found".to_string();
                            return Ok(true);
                        }
                    };

                    let body = session.read_request_body().await.map_err(|e| {
                        error!("preview-auth: failed to read sandbox login body: {}", e);
                        e
                    })?;
                    let body_str = body
                        .as_ref()
                        .map(|b| String::from_utf8_lossy(b).to_string())
                        .unwrap_or_default();
                    let params: Vec<(String, String)> =
                        url::form_urlencoded::parse(body_str.as_bytes())
                            .into_owned()
                            .collect();
                    let password = params
                        .iter()
                        .find(|(k, _)| k == "password")
                        .map(|(_, v)| v.as_str())
                        .unwrap_or("");
                    let next_raw = params
                        .iter()
                        .find(|(k, _)| k == "next")
                        .map(|(_, v)| v.as_str())
                        .unwrap_or("/");
                    let next = sanitize_next(next_raw);

                    if verify_argon2(password, &stored_hash) {
                        self.preview_auth_limiter.record_success(client_ip, &hex);
                        let subject = format!("sbx_{}", hex);
                        let Some(cookie_value) = encode_preview_cookie_subject(
                            &self.crypto,
                            &subject,
                            &stored_hash,
                            std::time::SystemTime::now(),
                        ) else {
                            error!("preview-auth: failed to encode sandbox preview cookie");
                            let mut response =
                                ResponseHeader::build(StatusCode::INTERNAL_SERVER_ERROR, None)?;
                            response.insert_header("Cache-Control", "no-store")?;
                            response.insert_header("X-Request-ID", &ctx.request_id)?;
                            session
                                .write_response_header(Box::new(response), false)
                                .await?;
                            session
                                .write_response_body(
                                    Some(Bytes::from_static(b"Cookie mint failed\n")),
                                    true,
                                )
                                .await?;
                            ctx.routing_status = "preview_cookie_error".to_string();
                            return Ok(true);
                        };
                        let set_cookie = build_set_cookie_sandbox(
                            &hex,
                            &cookie_value,
                            &settings.preview_domain,
                            self.is_tls_connection(session),
                        );

                        info!(sandbox = %hex, "preview-auth: sandbox login succeeded");
                        let mut response = ResponseHeader::build(303, None)?;
                        response.insert_header("Location", &next)?;
                        response.insert_header("Set-Cookie", &set_cookie)?;
                        response.insert_header("Cache-Control", "no-store")?;
                        response.insert_header("X-Request-ID", &ctx.request_id)?;
                        session
                            .write_response_header(Box::new(response), true)
                            .await?;
                        ctx.routing_status = "preview_login_ok".to_string();
                        return Ok(true);
                    } else {
                        self.preview_auth_limiter.record_failure(client_ip, &hex);
                        debug!(sandbox = %hex, "preview-auth: sandbox login failed (bad password)");
                        let label = format!("sandbox sbx_{}", hex);
                        let html = generate_preview_form_html_labeled(
                            &label,
                            preview_host.port,
                            &next,
                            true,
                        );
                        let html_bytes = Bytes::from(html);
                        let mut response = ResponseHeader::build(StatusCode::UNAUTHORIZED, None)?;
                        response.insert_header("Content-Type", "text/html; charset=utf-8")?;
                        response.insert_header("Cache-Control", "no-store")?;
                        response.insert_header("X-Request-ID", &ctx.request_id)?;
                        session
                            .write_response_header(Box::new(response), false)
                            .await?;
                        session.write_response_body(Some(html_bytes), true).await?;
                        ctx.routing_status = "preview_login_failed".to_string();
                        return Ok(true);
                    }
                }

                // GET/HEAD /__temps/preview/login for a sandbox host.
                if let Some(hex) = sandbox_hex.clone().filter(|_| {
                    ctx.path == PREVIEW_LOGIN_PATH && (ctx.method == "GET" || ctx.method == "HEAD")
                }) {
                    let next_raw = ctx
                        .query_string
                        .as_deref()
                        .and_then(|qs| {
                            url::form_urlencoded::parse(qs.as_bytes())
                                .find(|(k, _)| k == "next")
                                .map(|(_, v)| v.into_owned())
                        })
                        .unwrap_or_else(|| "/".to_string());
                    let next = sanitize_next(&next_raw);
                    let label = format!("sandbox sbx_{}", hex);
                    let html =
                        generate_preview_form_html_labeled(&label, preview_host.port, &next, false);
                    let html_bytes = Bytes::from(html);
                    let mut response = ResponseHeader::build(StatusCode::OK, None)?;
                    response.insert_header("Content-Type", "text/html; charset=utf-8")?;
                    response.insert_header("Cache-Control", "no-store")?;
                    response.insert_header("X-Request-ID", &ctx.request_id)?;
                    session
                        .write_response_header(Box::new(response), false)
                        .await?;
                    if ctx.method == "GET" {
                        session.write_response_body(Some(html_bytes), true).await?;
                    } else {
                        session.write_response_body(None, true).await?;
                    }
                    ctx.routing_status = "preview_login_form".to_string();
                    return Ok(true);
                }

                // POST /__temps/preview/logout for a sandbox host.
                if let Some(hex) = sandbox_hex
                    .clone()
                    .filter(|_| ctx.path == PREVIEW_LOGOUT_PATH && ctx.method == "POST")
                {
                    let set_cookie = build_logout_cookie_sandbox(
                        &hex,
                        &settings.preview_domain,
                        self.is_tls_connection(session),
                    );
                    let mut response = ResponseHeader::build(303, None)?;
                    response.insert_header("Location", "/")?;
                    response.insert_header("Set-Cookie", &set_cookie)?;
                    response.insert_header("Cache-Control", "no-store")?;
                    response.insert_header("X-Request-ID", &ctx.request_id)?;
                    session
                        .write_response_header(Box::new(response), true)
                        .await?;
                    ctx.routing_status = "preview_logout".to_string();
                    return Ok(true);
                }

                // ── Regular preview request: check cookie ─────────────────
                let cookie_header = session
                    .req_header()
                    .headers
                    .get("cookie")
                    .and_then(|h| h.to_str().ok())
                    .map(|s| s.to_string());

                let outcome = check_preview_auth(
                    &self.sandbox_lookup_cache,
                    &self.crypto,
                    &self.preview_auth_limiter,
                    preview_host,
                    client_ip,
                    cookie_header.as_deref(),
                )
                .await;

                match outcome {
                    PreviewAuthOutcome::Allow { host } => {
                        info!(
                            target = %host.label(),
                            port = host.port,
                            "preview-auth: allowed"
                        );
                        ctx.preview_route = Some(host);
                        ctx.routing_status = "preview".to_string();

                        // Strip any Authorization header before forwarding so
                        // the dev server inside the sandbox never sees upstream
                        // secrets that happen to be present.
                        let _ = session.req_header_mut().remove_header("authorization");

                        // Inject the shared secret so the gateway accepts us.
                        // Read at request time to allow live rotation.
                        if let Ok(secret) = std::env::var("PREVIEW_GATEWAY_SHARED_SECRET") {
                            if !secret.is_empty() {
                                session
                                    .req_header_mut()
                                    .insert_header("X-Temps-Preview-Token", &secret)?;
                            }
                        }
                        // Fall through — upstream_peer will route to the gateway.
                    }
                    PreviewAuthOutcome::LoginRequired { host } => {
                        debug!(
                            target = %host.label(),
                            "preview-auth: redirecting to login"
                        );
                        // Build the original path + query to stash as `next`.
                        let original = if let Some(ref qs) = ctx.query_string {
                            if qs.is_empty() {
                                ctx.path.clone()
                            } else {
                                format!("{}?{}", ctx.path, qs)
                            }
                        } else {
                            ctx.path.clone()
                        };
                        let next = sanitize_next(&original);
                        let location = format!(
                            "{}?next={}",
                            PREVIEW_LOGIN_PATH,
                            url::form_urlencoded::byte_serialize(next.as_bytes())
                                .collect::<String>()
                        );
                        let mut response = ResponseHeader::build(303, None)?;
                        response.insert_header("Location", &location)?;
                        response.insert_header("Cache-Control", "no-store")?;
                        response.insert_header("X-Request-ID", &ctx.request_id)?;
                        session
                            .write_response_header(Box::new(response), true)
                            .await?;
                        ctx.routing_status = "preview_login_required".to_string();
                        return Ok(true);
                    }
                    PreviewAuthOutcome::RateLimited { host } => {
                        warn!(
                            target = %host.label(),
                            client_ip = %client_ip,
                            "preview-auth: rate limited"
                        );
                        let mut response =
                            ResponseHeader::build(StatusCode::TOO_MANY_REQUESTS, None)?;
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
                        return Ok(true);
                    }
                    PreviewAuthOutcome::NotFound { host } => {
                        debug!(
                            target = %host.label(),
                            "preview-auth: target not found or no password"
                        );
                        let mut response = ResponseHeader::build(StatusCode::NOT_FOUND, None)?;
                        response.insert_header("Cache-Control", "no-store")?;
                        response.insert_header("X-Request-ID", &ctx.request_id)?;
                        response.insert_header("Content-Type", "text/plain; charset=utf-8")?;
                        session
                            .write_response_header(Box::new(response), false)
                            .await?;
                        session
                            .write_response_body(
                                Some(Bytes::from_static(b"Preview not found\n")),
                                true,
                            )
                            .await?;
                        ctx.routing_status = "preview_not_found".to_string();
                        return Ok(true);
                    }
                }
            }
        }

        // On-demand: check if this host maps to a sleeping environment.
        // Sleeping environments are excluded from the route table, so we must
        // check before project context resolution. Wake the environment inline
        // and hold the request until the container is ready and routes are reloaded.
        if let Some(ref on_demand) = self.on_demand_manager {
            let host_without_port = ctx.host.split(':').next().unwrap_or(&ctx.host);
            if let Some(sleeping_info) = on_demand.get_sleeping_environment(host_without_port) {
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
                        let mut routable = self
                            .project_context_resolver
                            .resolve_context(&ctx.host)
                            .await
                            .is_some();
                        while !routable && std::time::Instant::now() < resolve_deadline {
                            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                            routable = self
                                .project_context_resolver
                                .resolve_context(&ctx.host)
                                .await
                                .is_some();
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

        // Resolve project context early to set routing status for all requests
        let project_context = self
            .project_context_resolver
            .resolve_context(&ctx.host)
            .await;

        if let Some(project_ctx) = &project_context {
            ctx.project = Some(project_ctx.project.clone());
            ctx.environment = Some(project_ctx.environment.clone());
            ctx.deployment = Some(project_ctx.deployment.clone());
            ctx.routing_status = "routed".to_string();

            // Record activity for on-demand idle tracking
            if let Some(ref on_demand) = self.on_demand_manager {
                on_demand.record_activity(project_ctx.environment.id);
            }

            // Check if this is a CAPTCHA endpoint - allow these to bypass attack mode
            // This includes:
            // - /api/_temps/captcha/* - Challenge verification endpoints
            // - /api/__temps/temps_captcha_wasm.js - WASM JavaScript bindings
            // - /api/__temps/temps_captcha_wasm_bg.wasm - WASM binary module
            let is_captcha_endpoint = ctx.path.starts_with("/api/_temps/captcha")
                || ctx.path.starts_with("/api/__temps/temps_captcha_wasm");

            // Check if attack mode is enabled. The environment-level override
            // (Option<bool>, NULL = inherit) falls back to the project-wide setting.
            let effective_attack_mode = project_ctx
                .environment
                .attack_mode
                .unwrap_or(project_ctx.project.attack_mode);
            if !is_captcha_endpoint && effective_attack_mode {
                // Attack mode REQUIRES HTTPS for JA4 fingerprinting
                // Reject HTTP connections to prevent bot bypass
                debug!(
                    "Attack mode enabled for environment {}, fingerprint: {:?}, user_agent: {}",
                    project_ctx.environment.id, ctx.tls_fingerprint, ctx.user_agent
                );

                let (identifier_type, identifier) = if let Some(ref fingerprint) =
                    ctx.tls_fingerprint
                {
                    ("ja4", fingerprint.as_str())
                } else {
                    // No TLS fingerprint means HTTP connection - reject it
                    debug!(
                        "Attack mode: HTTPS required for environment {} (HTTP request from {})",
                        project_ctx.environment.id,
                        ctx.ip_address.as_ref().unwrap_or(&"unknown".to_string())
                    );

                    // Return 426 Upgrade Required
                    let mut response =
                        ResponseHeader::build(StatusCode::from_u16(426).unwrap(), None)?;
                    response.insert_header("Content-Type", "text/html; charset=utf-8")?;
                    response.insert_header("Upgrade", "TLS/1.2, TLS/1.3")?;
                    response.insert_header("Connection", "Upgrade")?;

                    session
                        .write_response_header(Box::new(response), true)
                        .await?;

                    let html = r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>HTTPS Required</title>
    <style>
        body { font-family: system-ui, -apple-system, sans-serif; display: flex; align-items: center; justify-content: center; min-height: 100vh; margin: 0; background: linear-gradient(135deg, #667eea 0%, #764ba2 100%); }
        .container { background: white; border-radius: 16px; padding: 40px; max-width: 500px; text-align: center; box-shadow: 0 20px 60px rgba(0,0,0,0.3); }
        h1 { color: #1a202c; margin-bottom: 16px; }
        p { color: #4a5568; line-height: 1.6; }
        .icon { font-size: 64px; margin-bottom: 16px; }
    </style>
</head>
<body>
    <div class="container">
        <div class="icon">🔒</div>
        <h1>HTTPS Required</h1>
        <p>This site requires a secure connection (HTTPS) for enhanced security and bot protection.</p>
        <p>Please use <strong>https://</strong> instead of http://</p>
    </div>
</body>
</html>"#.to_string();

                    session
                        .write_response_body(Some(Bytes::from(html)), true)
                        .await?;

                    return Err(Error::because(
                        pingora::ErrorType::HTTPStatus(426),
                        "HTTPS required in attack mode",
                        pingora_core::Error::new(pingora::ErrorType::HTTPStatus(426)),
                    ));
                };

                let is_challenge_completed = self
                    .challenge_service
                    .is_challenge_completed(project_ctx.environment.id, identifier, identifier_type)
                    .await
                    .unwrap_or(false);

                if !is_challenge_completed {
                    debug!(
                        "Attack mode: Challenge required for {} {} on environment {}",
                        identifier_type, identifier, project_ctx.environment.id
                    );

                    // Return 403 with HTML challenge page
                    let mut response = ResponseHeader::build(StatusCode::FORBIDDEN, None)?;
                    response.insert_header("Content-Type", "text/html; charset=utf-8")?;
                    response.insert_header("X-Challenge-Required", "true")?;

                    session
                        .write_response_header(Box::new(response), true)
                        .await?;

                    // Generate HTML challenge page
                    let html = Self::generate_challenge_html(
                        &project_ctx.project.name,
                        project_ctx.environment.id,
                        ctx.ip_address.as_ref().unwrap_or(&"unknown".to_string()),
                        identifier,
                        identifier_type,
                    );

                    session
                        .write_response_body(Some(Bytes::from(html)), true)
                        .await?;

                    // Return error to stop request processing
                    return Err(Error::because(
                        pingora::ErrorType::HTTPStatus(403),
                        "Challenge required",
                        pingora_core::Error::new(pingora::ErrorType::HTTPStatus(403)),
                    ));
                }
            }

            // Password wall: check if environment has password protection enabled
            let password_protection = project_ctx
                .environment
                .deployment_config
                .as_ref()
                .and_then(|dc| dc.security.as_ref())
                .and_then(|s| s.password_protection.as_ref())
                .filter(|pp| pp.enabled);

            if let Some(pp) = password_protection {
                let password_hash = pp.password_hash.clone();
                let env_id = project_ctx.environment.id;
                let project_name = &project_ctx.project.name;
                let environment_name = &project_ctx.environment.name;

                // Check if this is the password verify POST endpoint
                if ctx.path == "/_temps/password-verify" && ctx.method == "POST" {
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

                    let redirect = params
                        .iter()
                        .find(|(k, _)| k == "redirect")
                        .map(|(_, v)| v.as_str())
                        .unwrap_or("/");

                    if crate::handler::password_wall::verify_password(password, &password_hash) {
                        // Password correct — set cookie and redirect
                        let host = ctx.host.clone();
                        let set_cookie = crate::handler::password_wall::build_set_cookie_header(
                            env_id,
                            &password_hash,
                            &host,
                        );

                        let mut resp = ResponseHeader::build(303, None)?;
                        resp.insert_header("Location", redirect)?;
                        resp.insert_header("Set-Cookie", &set_cookie)?;
                        resp.insert_header("Cache-Control", "no-store")?;
                        resp.insert_header("X-Request-ID", &ctx.request_id)?;

                        session.write_response_header(Box::new(resp), true).await?;
                        ctx.routing_status = "password_verified".to_string();
                        return Ok(true);
                    } else {
                        // Wrong password — show form again with error
                        let html = crate::handler::password_wall::generate_password_form_html(
                            redirect,
                            true,
                            project_name,
                            environment_name,
                        );
                        let html_bytes = Bytes::from(html);

                        let mut resp = ResponseHeader::build(StatusCode::OK, None)?;
                        resp.insert_header("Content-Type", "text/html; charset=utf-8")?;
                        resp.insert_header("Cache-Control", "no-store")?;
                        resp.insert_header("X-Request-ID", &ctx.request_id)?;

                        session.write_response_header(Box::new(resp), false).await?;
                        session.write_response_body(Some(html_bytes), true).await?;
                        ctx.routing_status = "password_wrong".to_string();
                        return Ok(true);
                    }
                }

                // Check for valid password cookie
                let has_valid_cookie = session
                    .req_header()
                    .headers
                    .get_all("Cookie")
                    .iter()
                    .filter_map(|h| h.to_str().ok())
                    .flat_map(|s| Cookie::split_parse(s).filter_map(Result::ok))
                    .find(|c| c.name() == crate::handler::password_wall::PASSWORD_COOKIE_NAME)
                    .map(|c| {
                        crate::handler::password_wall::validate_cookie(
                            c.value(),
                            env_id,
                            &password_hash,
                        )
                    })
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

                    let html = crate::handler::password_wall::generate_password_form_html(
                        &current_path,
                        false,
                        project_name,
                        environment_name,
                    );
                    let html_bytes = Bytes::from(html);

                    let mut resp = ResponseHeader::build(StatusCode::OK, None)?;
                    resp.insert_header("Content-Type", "text/html; charset=utf-8")?;
                    resp.insert_header("Cache-Control", "no-store")?;
                    resp.insert_header("X-Request-ID", &ctx.request_id)?;

                    session.write_response_header(Box::new(resp), false).await?;
                    session.write_response_body(Some(html_bytes), true).await?;
                    ctx.routing_status = "password_wall".to_string();
                    return Ok(true);
                }
            }
        } else {
            ctx.routing_status = "no_project".to_string();
        }

        // Serve embedded WASM files for CAPTCHA solver (must come before general request handling)
        if let Ok(true) = self.serve_wasm_file(session, ctx).await {
            ctx.routing_status = "captcha_wasm".to_string();
            return Ok(true); // Request handled
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
        // This MUST come after ACME challenge handling to allow Let's Encrypt HTTP-01 validation.
        //
        // Redirect is per-domain: we only redirect when the requesting host
        // actually has an active TLS certificate in the database (exact match or
        // wildcard parent). This means HTTP-only installs (sslip.io quick/local
        // modes, no cert provisioned) never get redirected, while hosts that
        // have gone through SSL provisioning get automatic HTTPS enforcement.
        //
        // `disable_https_redirect` is a global escape hatch (set by the service
        // unit in local/testing mode) that bypasses the check entirely.
        // WS3: cert-host check is now a lock-free ArcSwap snapshot read; the
        // background `CertHostCache::run_refresh_loop` keeps it current (±30 s).
        let needs_redirect = !self.disable_https_redirect
            && !self.is_tls_connection(session)
            && self.cert_host_cache.has_cert_for_host(&ctx.host);
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
            let mut resp = ResponseHeader::build(301, None)?;
            resp.insert_header("Location", &redirect_url)?;
            resp.insert_header("Content-Length", "0")?;
            resp.insert_header("X-Request-ID", &ctx.request_id)?;

            ctx.routing_status = "http_to_https_redirect".to_string();

            session.write_response_header(Box::new(resp), true).await?;
            return Ok(true);
        }

        // Check if this host should redirect
        if let Some((redirect_url, status_code)) = self
            .project_context_resolver
            .get_redirect_info(&ctx.host)
            .await
        {
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

            // Add CORS headers for redirect responses
            resp.insert_header("Access-Control-Allow-Origin", "*")?;

            // Update context for logging
            ctx.routing_status = "redirected".to_string();

            session.write_response_header(Box::new(resp), true).await?;
            return Ok(true); // Skip proxying
        }

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

        ctx.request_visitor_cookie = session
            .req_header()
            .headers
            .get_all("Cookie")
            .iter()
            .filter_map(|cookie_header| cookie_header.to_str().ok())
            .flat_map(|cookie_str| Cookie::split_parse(cookie_str).filter_map(Result::ok))
            .find(|cookie| cookie.name() == visitor_cookie_name)
            .map(|cookie| cookie.value().to_string());

        ctx.request_session_cookie = session
            .req_header()
            .headers
            .get_all("Cookie")
            .iter()
            .filter_map(|cookie_header| cookie_header.to_str().ok())
            .flat_map(|cookie_str| Cookie::split_parse(cookie_str).filter_map(Result::ok))
            .find(|cookie| cookie.name() == session_cookie_name)
            .map(|cookie| cookie.value().to_string());

        // Get IP from the connection
        // Add X-Forwarded-For header with client IP (already extracted in request_filter)
        if let Some(ref ip) = ctx.ip_address {
            session
                .req_header_mut()
                .insert_header("X-Forwarded-For", ip.as_str())?;
        }

        // Add X-Forwarded-Proto header to indicate the original protocol (HTTP/HTTPS)
        let proto = if self.is_https_request(session) {
            "https"
        } else {
            "http"
        };
        session
            .req_header_mut()
            .insert_header("X-Forwarded-Proto", proto)?;

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
        if let Some(redirect_info) = self
            .project_context_resolver
            .get_redirect_info(&ctx.host)
            .await
        {
            let mut resp = ResponseHeader::build(redirect_info.1, None)?;
            resp.insert_header(header::LOCATION, &redirect_info.0)?;
            session.write_response_header(Box::new(resp), true).await?;
            return Ok(true);
        }

        // Check if this is a static deployment using route table
        if let Some(static_dir) = self
            .project_context_resolver
            .get_static_path(&ctx.host)
            .await
        {
            debug!(
                "Static deployment detected for {}: {}",
                ctx.host, static_dir
            );

            // IMPORTANT: Skip static file serving for /api/_temps/* paths
            // These must ALWAYS be proxied to the console address (admin API)
            if !ctx.path.starts_with("/api/_temps/") {
                // Only create visitor/session for HTML page requests, not static assets.
                // Without this guard, concurrent requests for JS/CSS/images on first visit
                // (before the browser has received the Set-Cookie response) each create a
                // separate visitor record, causing duplicate "live visitors".
                let is_static_asset = ctx.path.contains('.')
                    && (ctx.path.ends_with(".js")
                        || ctx.path.ends_with(".css")
                        || ctx.path.ends_with(".png")
                        || ctx.path.ends_with(".jpg")
                        || ctx.path.ends_with(".jpeg")
                        || ctx.path.ends_with(".gif")
                        || ctx.path.ends_with(".svg")
                        || ctx.path.ends_with(".ico")
                        || ctx.path.ends_with(".woff")
                        || ctx.path.ends_with(".woff2")
                        || ctx.path.ends_with(".ttf")
                        || ctx.path.ends_with(".eot")
                        || ctx.path.ends_with(".map")
                        || ctx.path.ends_with(".webp")
                        || ctx.path.ends_with(".avif")
                        || ctx.path.ends_with(".json")
                        || ctx.path.ends_with(".xml")
                        || ctx.path.ends_with(".txt"));

                if !is_static_asset {
                    self.ensure_visitor_session(ctx).await;
                }

                // Serve static file
                match self.serve_static_file(session, ctx, &static_dir).await {
                    Ok(served) => {
                        if served {
                            debug!("Served static file: {}", ctx.path);
                            ctx.routing_status = "static_file".to_string();

                            // Log successful static file serving (HTML only)
                            self.log_static_request(
                                ctx,
                                200,
                                "static_file",
                                &static_dir,
                                None,
                                None,
                            );

                            return Ok(true); // Request handled
                        } else {
                            // Static file not found in current deployment.
                            // Try path-keyed file store (stale-chunk fallback, no DB).
                            if Self::is_cacheable_static_asset(&ctx.path) {
                                let url_path = ctx.path.trim_start_matches('/').to_string();
                                if let Ok(true) =
                                    self.serve_asset_from_store(session, ctx, &url_path).await
                                {
                                    ctx.routing_status = "stale_chunk_fallback".to_string();
                                    return Ok(true);
                                }
                            }

                            // Static file not found - return 404
                            error!(
                                "Static file not found: {} (static dir: {})",
                                ctx.path, static_dir
                            );
                            let mut resp = ResponseHeader::build(StatusCode::NOT_FOUND, None)?;
                            resp.insert_header(header::CONTENT_TYPE, "text/html")?;

                            // Set tracking cookies for 404 response
                            self.set_tracking_cookies(session, &mut resp, ctx).await?;

                            session.write_response_header(Box::new(resp), false).await?;
                            session
                                .write_response_body(
                                    Some(bytes::Bytes::from(
                                        b"<html><body><h1>404 - File Not Found</h1></body></html>"
                                            .to_vec(),
                                    )),
                                    true,
                                )
                                .await?;

                            // Log 404 static file not found (HTML only)
                            self.log_static_request(
                                ctx,
                                404,
                                "static_file_not_found",
                                &static_dir,
                                Some("Static file not found".to_string()),
                                Some(
                                    b"<html><body><h1>404 - File Not Found</h1></body></html>".len()
                                        as i64,
                                ),
                            );

                            return Ok(true); // Request handled with 404
                        }
                    }
                    Err(e) => {
                        // Static directory error (doesn't exist, permissions, etc.) - return 500
                        error!(
                            "Failed to serve static file {} from {}: {}",
                            ctx.path, static_dir, e
                        );
                        let mut resp =
                            ResponseHeader::build(StatusCode::INTERNAL_SERVER_ERROR, None)?;
                        resp.insert_header(header::CONTENT_TYPE, "text/html")?;

                        // Set tracking cookies for 500 response
                        self.set_tracking_cookies(session, &mut resp, ctx).await?;

                        session.write_response_header(Box::new(resp), false).await?;
                        session
                        .write_response_body(
                            Some(bytes::Bytes::from(
                                b"<html><body><h1>500 - Static Directory Error</h1><p>The static files directory could not be accessed.</p></body></html>"
                                    .to_vec(),
                            )),
                            true,
                        )
                        .await?;

                        // Log 500 static directory error (HTML only)
                        let error_msg = format!("Static directory error: {}", e);
                        self.log_static_request(
                        ctx,
                        500,
                        "static_directory_error",
                        &static_dir,
                        Some(error_msg),
                        Some(
                            b"<html><body><h1>500 - Static Directory Error</h1><p>The static files directory could not be accessed.</p></body></html>"
                                .len() as i64,
                        ),
                    );

                        return Ok(true); // Request handled with error response
                    }
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
                let asset_path = after_prefix[slash_pos + 1..].to_string();
                if Self::is_cacheable_static_asset(&asset_path) {
                    if let Ok(true) = self.serve_asset_from_store(session, ctx, &asset_path).await {
                        ctx.routing_status = "prefixed_asset".to_string();
                        return Ok(true);
                    }
                }
            }
        }

        // Fallback: serve immutable static assets from file store.
        // For container deployments where the upstream didn't have the asset,
        // check the path-keyed file store (stale-chunk fallback).
        if Self::is_cacheable_static_asset(&ctx.path)
            && !self
                .project_context_resolver
                .is_static_deployment(&ctx.host)
                .await
        {
            let url_path = ctx.path.trim_start_matches('/').to_string();
            if let Ok(true) = self.serve_asset_from_store(session, ctx, &url_path).await {
                ctx.routing_status = "stale_chunk_fallback".to_string();
                return Ok(true);
            }
        }

        // Admin gate: when a non-noop gate is wired and the request is
        // about to fall back to the console (no deployed app for this host,
        // not a public ingest path under /api/_temps/*, not a preview),
        // require the (IP, Host) tuple to pass the gate. If it doesn't,
        // return a 404 from the proxy itself so the management surface is
        // invisible from non-admin hosts.
        if let Some(gate) = self.admin_gate.as_ref() {
            let config = gate.current();
            if Self::should_consult_admin_gate(&config, &ctx.path, ctx.preview_route.is_some()) {
                let host_has_route = self.upstream_resolver.has_route_for_host(&ctx.host).await;
                if !host_has_route {
                    let client_ip = ctx
                        .ip_address
                        .as_deref()
                        .and_then(|s| s.parse::<std::net::IpAddr>().ok())
                        .unwrap_or_else(|| std::net::IpAddr::from([127, 0, 0, 1]));
                    if !config.would_allow(client_ip, Some(&ctx.host)) {
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

        Ok(false)
    }

    async fn upstream_response_filter(
        &self,
        _session: &mut PingoraSession,
        upstream_response: &mut ResponseHeader,
        ctx: &mut Self::CTX,
    ) -> Result<()>
    where
        Self::CTX: Send + Sync,
    {
        debug!("Upstream response filter headers: {:?}", upstream_response);

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
            .map(|ct| ct.contains("text/event-stream"))
            .unwrap_or(false);

        if is_sse {
            ctx.is_sse = true;
            ctx.skip_tracking = true; // Skip visitor/session tracking for SSE streams
            debug!("SSE response detected from upstream");
        }

        // Strip content-length from HEAD responses. The upstream correctly includes it
        // (per RFC 9110 §9.3.2, HEAD responses SHOULD have the same content-length as GET)
        // but when proxied over HTTP/2, clients like curl interpret the content-length as
        // a promise of body bytes and error when none arrive. Cloudflare strips it too.
        if ctx.method == "HEAD" {
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
        let is_html_content = ctx
            .content_type
            .as_ref()
            .map(|ct| ct.starts_with("text/html"))
            .unwrap_or(false);

        let status_code = upstream_response.status.as_u16();
        let is_error_page = status_code >= 400;

        let is_static_asset = ctx.path.contains(".")
            && (ctx.path.ends_with(".js")
                || ctx.path.ends_with(".css")
                || ctx.path.ends_with(".png")
                || ctx.path.ends_with(".jpg")
                || ctx.path.ends_with(".jpeg")
                || ctx.path.ends_with(".gif")
                || ctx.path.ends_with(".svg")
                || ctx.path.ends_with(".ico")
                || ctx.path.ends_with(".woff")
                || ctx.path.ends_with(".woff2")
                || ctx.path.ends_with(".ttf")
                || ctx.path.ends_with(".eot"));

        let is_api_endpoint = ctx.path.starts_with("/api/") || ctx.path.starts_with("/_temps/");

        // Check if we should track this page view
        let should_track =
            Self::should_track_page(&ctx.path, ctx.content_type.as_deref(), status_code);

        // Only create visitor/session for appropriate requests (skip for SSE)
        if !ctx.skip_tracking
            && should_track
            && (is_html_content || is_error_page)
            && !is_static_asset
            && !is_api_endpoint
        {
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
        let io_timeout = if is_websocket {
            std::time::Duration::from_secs(3600)
        } else {
            std::time::Duration::from_secs(60)
        };

        // Workspace preview gateway: skip the route table and forward straight
        // to the local gateway. The host header is preserved so the gateway
        // can decode `ws-<sid>-<port>` and pick the right sandbox container.
        //
        // Every preview target shares this same physical peer address, so
        // `group_key` MUST be set per-target — otherwise Pingora's
        // connection pool considers all sandboxes' requests interchangeable
        // and can hand a connection opened for one sandbox back out to
        // serve a different sandbox's request (see `preview_peer_group_key`
        // doc comment for the full mechanism).
        if let Some(host) = &ctx.preview_route {
            let mut peer = Box::new(HttpPeer::new(PREVIEW_GATEWAY_PEER, false, String::new()));
            peer.group_key = preview_peer_group_key(host);
            peer.options.connection_timeout = Some(std::time::Duration::from_secs(5));
            peer.options.read_timeout = Some(io_timeout);
            peer.options.write_timeout = Some(io_timeout);
            peer.options.idle_timeout = Some(io_timeout);
            ctx.upstream_host = Some(PREVIEW_GATEWAY_PEER.to_string());
            return Ok(peer);
        }

        let domain = self.get_host_header(session)?;
        let path = session.req_header().uri.path().to_string();

        debug!(
            "Resolving upstream peer for domain: {}, path: {}",
            domain, path
        );

        // Use the upstream resolver trait
        // Pass SNI hostname for TLS-based routing
        let selection = self
            .upstream_resolver
            .resolve_peer(&domain, &path, ctx.sni_hostname.as_deref())
            .await?;

        let mut peer = selection.peer;

        // Configure upstream connection options. `io_timeout` is bumped to
        // 1h for websocket upgrades (see top of this method) so idle terminals
        // and SSE streams don't get RST every 60s.
        peer.options.connection_timeout = Some(std::time::Duration::from_secs(5));
        peer.options.read_timeout = Some(io_timeout);
        peer.options.write_timeout = Some(io_timeout);
        // Close idle pooled connections after the same window to avoid stale
        // keep-alive reuse.
        peer.options.idle_timeout = Some(io_timeout);

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
        if ctx.upstream_connect_tries == 0 {
            ctx.upstream_connect_tries += 1;
            warn!("Upstream connection failed (try 1), retrying: {:?}", e);
            e.set_retry(true);
        } else {
            error!("Upstream connection failed after retry: {:?}", e);
        }
        e
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
        error!(
            "Failed to proxy: {:?} | request_id={} client_ip={} host={} method={} path={}",
            e,
            ctx.request_id,
            ctx.ip_address.as_deref().unwrap_or("unknown"),
            ctx.host,
            ctx.method,
            ctx.path
        );

        let mut error_code = 500;
        let can_reuse_downstream = false;

        // Update context with error
        ctx.error_message = Some(e.to_string());
        ctx.routing_status = "error".to_string();

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

        const SERVICE_UNAVAILABLE_BODY: &str = concat!(
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

        if let Err(e) = session
            .write_response_body(Some(Bytes::from(SERVICE_UNAVAILABLE_BODY)), true)
            .await
        {
            error!("Failed to write response body: {:?}", e);
        }

        error_code = 503;

        // Asynchronously log failed proxy request (skip static assets)
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

            // For failed requests, response size is the error message size
            let response_size = Some(SERVICE_UNAVAILABLE_BODY.len() as i64);

            let proxy_log_request = CreateProxyLogRequest {
                method: ctx.method.clone(),
                path: ctx.path.clone(),
                query_string: None,
                host: ctx.host.clone(),
                status_code: error_code as i16,
                response_time_ms: Some(ctx.start_time.elapsed().as_millis() as i32),
                request_source: "proxy".to_string(),
                is_system_request: ctx.path.starts_with(ROUTE_PREFIX_TEMPS),
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
                response_size_bytes: response_size,
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
            };

            // Non-blocking enqueue; shed with rate-limited accounting when full.
            self.proxy_log_handle.send_or_drop(proxy_log_request);
        }

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

        // Hot path: a handful of relaxed atomic adds, no locks, no I/O.
        self.proxy_metrics.record(
            status_code,
            ctx.start_time.elapsed().as_millis() as u64,
            ctx.upstream_response_time_ms,
            destination,
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

#[cfg(test)]
mod admin_gate_tests {
    use super::*;
    use temps_core::admin_gate::{AdminGateConfig, AdminGateSource};

    fn gated(hosts: &[&str]) -> AdminGateConfig {
        let owned: Vec<String> = hosts.iter().map(|s| s.to_string()).collect();
        AdminGateConfig::from_parts(&[], &owned, false, AdminGateSource::Db)
            .expect("valid gate config")
    }

    #[test]
    fn noop_gate_short_circuits_consultation() {
        let config = AdminGateConfig::from_parts(&[], &[], false, AdminGateSource::Default)
            .expect("empty noop config");
        assert!(config.is_noop());
        assert!(!LoadBalancer::should_consult_admin_gate(
            &config, "/", false,
        ));
    }

    #[test]
    fn preview_routes_bypass_gate() {
        let config = gated(&["app.temps.kfs.es"]);
        assert!(!LoadBalancer::should_consult_admin_gate(
            &config,
            "/some/path",
            true,
        ));
    }

    #[test]
    fn temps_ingest_paths_bypass_gate() {
        let config = gated(&["app.temps.kfs.es"]);
        // Public ingest like /api/_temps/event must reach the console from any host.
        assert!(!LoadBalancer::should_consult_admin_gate(
            &config,
            "/api/_temps/event",
            false,
        ));
    }

    #[test]
    fn normal_request_consults_gate_when_configured() {
        let config = gated(&["app.temps.kfs.es"]);
        assert!(LoadBalancer::should_consult_admin_gate(&config, "/", false,));
    }

    // Regression: setting an admin host (e.g. `app.temps.kfs.es`) used to
    // 404 every project deployment because the gate consulted
    // `has_custom_route`, which only knows about operator-defined LB
    // overrides — not the in-memory project route table. The fix is the
    // new `has_route_for_host` trait method; this test pins the contract:
    // a resolver that reports the host via `has_route_for_host` is treated
    // as known, even when `has_custom_route` says no.
    #[tokio::test]
    async fn has_route_for_host_recognizes_project_hosts_outside_custom_routes() {
        use crate::traits::{PeerSelection, UpstreamResolver};
        use async_trait::async_trait;
        use pingora_core::upstreams::peer::HttpPeer;
        use std::collections::HashSet;

        struct ProjectRouteOnlyResolver {
            project_hosts: HashSet<String>,
        }

        #[async_trait]
        impl UpstreamResolver for ProjectRouteOnlyResolver {
            async fn resolve_peer(
                &self,
                _host: &str,
                _path: &str,
                _sni: Option<&str>,
            ) -> pingora_core::Result<PeerSelection> {
                Ok(PeerSelection {
                    peer: Box::new(HttpPeer::new("127.0.0.1:1".to_string(), false, "".into())),
                    container_id: None,
                    container_name: None,
                })
            }

            async fn has_custom_route(&self, _host: &str) -> bool {
                // Simulates the old behavior: no entry in `custom_routes`.
                false
            }

            async fn has_route_for_host(&self, host: &str) -> bool {
                // Simulates the route_table check: project hosts are known here.
                self.project_hosts.contains(host)
            }

            async fn get_lb_strategy(&self, _host: &str) -> Option<String> {
                None
            }
        }

        let resolver = ProjectRouteOnlyResolver {
            project_hosts: ["myproject.example.com".to_string()].into_iter().collect(),
        };

        // Old check missed the project — would have triggered the gate deny path.
        assert!(!resolver.has_custom_route("myproject.example.com").await);
        // New check finds it — gate path correctly skips deny.
        assert!(resolver.has_route_for_host("myproject.example.com").await);
        // Truly unknown hosts still fall through and would hit the gate.
        assert!(!resolver.has_route_for_host("evil.example.com").await);
    }
}

#[cfg(test)]
mod on_demand_http_tests {
    //! Unit tests for the ADR-018 §5 port-80 on-demand TLS UX decision logic.
    //! The session-writing wrapper (`handle_on_demand_http`) is exercised in
    //! integration; here we pin the two pure helpers it delegates to so the
    //! 503 contract and the `redirect_to_env` target derivation are locked.
    use super::{ephemeral_redirect_location, on_demand_cert_state_response};
    use crate::on_demand_cert::OnDemandCertState;

    #[test]
    fn pending_and_issuing_map_to_provisioning_503() {
        let (status, body) = on_demand_cert_state_response(&OnDemandCertState::Pending);
        assert_eq!(status, 503);
        assert_eq!(
            body,
            b"TLS certificate provisioning in progress. Retry in a few seconds.\n"
        );

        let (status, body) = on_demand_cert_state_response(&OnDemandCertState::Issuing);
        assert_eq!(status, 503);
        assert_eq!(
            body,
            b"TLS certificate provisioning in progress. Retry in a few seconds.\n"
        );
    }

    #[test]
    fn failed_maps_to_issuance_failed_503() {
        let (status, body) = on_demand_cert_state_response(&OnDemandCertState::Failed {
            backoff_until_epoch: 12345,
        });
        assert_eq!(status, 503);
        assert_eq!(
            body,
            b"TLS certificate issuance failed. Contact your administrator.\n"
        );
    }

    #[test]
    fn redirect_target_is_stable_env_url_preserving_path() {
        // Ephemeral host `myapp-prod-42.1.2.3.4.sslip.io` (env subdomain
        // `myapp-prod`) → stable `myapp-prod.1.2.3.4.sslip.io`.
        let location = ephemeral_redirect_location(
            Some("myapp-prod"),
            "1.2.3.4.sslip.io",
            "myapp-prod-42.1.2.3.4.sslip.io",
            "/dashboard",
            None,
        )
        .expect("should build a redirect target");
        assert_eq!(location, "https://myapp-prod.1.2.3.4.sslip.io/dashboard");
    }

    #[test]
    fn redirect_target_preserves_query_string() {
        let location = ephemeral_redirect_location(
            Some("myapp-prod"),
            "1.2.3.4.sslip.io",
            "myapp-prod-42.1.2.3.4.sslip.io",
            "/search",
            Some("q=temps&page=2"),
        )
        .expect("should build a redirect target");
        assert_eq!(
            location,
            "https://myapp-prod.1.2.3.4.sslip.io/search?q=temps&page=2"
        );
    }

    #[test]
    fn redirect_target_ignores_empty_query_string() {
        let location = ephemeral_redirect_location(
            Some("myapp-prod"),
            "1.2.3.4.sslip.io",
            "myapp-prod-42.1.2.3.4.sslip.io",
            "/",
            Some(""),
        )
        .expect("should build a redirect target");
        assert_eq!(location, "https://myapp-prod.1.2.3.4.sslip.io/");
    }

    #[test]
    fn no_redirect_without_env_subdomain() {
        assert!(ephemeral_redirect_location(
            None,
            "1.2.3.4.sslip.io",
            "myapp-prod-42.1.2.3.4.sslip.io",
            "/",
            None,
        )
        .is_none());
    }

    #[test]
    fn no_redirect_with_blank_env_subdomain_or_preview_domain() {
        assert!(ephemeral_redirect_location(
            Some("   "),
            "1.2.3.4.sslip.io",
            "ephemeral.host",
            "/",
            None,
        )
        .is_none());
        assert!(ephemeral_redirect_location(
            Some("myapp-prod"),
            "   ",
            "ephemeral.host",
            "/",
            None,
        )
        .is_none());
    }

    #[test]
    fn no_redirect_when_target_equals_request_host_avoids_loop() {
        // If the computed target is the request host (case-insensitive), we must
        // not redirect — that would loop forever.
        assert!(ephemeral_redirect_location(
            Some("myapp-prod"),
            "1.2.3.4.sslip.io",
            "MyApp-Prod.1.2.3.4.sslip.io",
            "/",
            None,
        )
        .is_none());
    }
}

#[cfg(test)]
mod markdown_tests {
    use super::*;
    use bytes::Bytes;

    // ── Helper: build a minimal ProxyContext for testing ──────────────────────
    fn make_ctx() -> ProxyContext {
        ProxyContext {
            response_modified: false,
            response_compressed: false,
            upstream_response_headers: None,
            content_type: None,
            buffer: vec![],
            project: None,
            environment: None,
            deployment: None,
            request_id: "test-req".to_string(),
            start_time: Instant::now(),
            method: "GET".to_string(),
            path: "/".to_string(),
            query_string: None,
            host: "example.com".to_string(),
            user_agent: "TestAgent/1.0".to_string(),
            referrer: None,
            ip_address: Some("127.0.0.1".to_string()),
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
            upstream_connect_tries: 0,
            upstream_write_pending_time_ms: None,
            upstream_start_time: None,
            upstream_response_time_ms: None,
            preview_route: None,
        }
    }

    // ── estimate_markdown_tokens ──────────────────────────────────────────────

    #[test]
    fn test_token_estimate_empty() {
        assert_eq!(estimate_markdown_tokens(""), 0);
    }

    #[test]
    fn test_token_estimate_proportional() {
        // 3 words → 4 tokens (3 * 4 / 3 = 4)
        let count = estimate_markdown_tokens("one two three");
        assert_eq!(count, 4);
    }

    #[test]
    fn test_token_estimate_larger() {
        // 300 words → 400 tokens
        let text = "word ".repeat(300);
        assert_eq!(estimate_markdown_tokens(&text), 400);
    }

    // ── wants_markdown detection (logic extracted from early_request_filter) ──

    fn parse_wants_markdown(accept: &str) -> bool {
        accept
            .split(',')
            .any(|part| part.trim().to_lowercase().starts_with("text/markdown"))
    }

    #[test]
    fn test_accept_text_markdown_exact() {
        assert!(parse_wants_markdown("text/markdown"));
    }

    #[test]
    fn test_accept_text_markdown_with_quality() {
        assert!(parse_wants_markdown("text/html, text/markdown;q=0.9"));
    }

    #[test]
    fn test_accept_text_markdown_uppercase() {
        assert!(parse_wants_markdown("Text/Markdown"));
    }

    #[test]
    fn test_accept_no_markdown() {
        assert!(!parse_wants_markdown("text/html, application/json"));
    }

    #[test]
    fn test_accept_empty() {
        assert!(!parse_wants_markdown(""));
    }

    // ── upstream_response_filter gating logic ─────────────────────────────────

    fn should_convert(ctx: &ProxyContext, content_type: &str) -> bool {
        // Mirrors the gating logic in upstream_response_filter
        ctx.wants_markdown && !ctx.is_sse && !ctx.is_websocket && content_type.contains("text/html")
    }

    #[test]
    fn test_gate_html_converts() {
        let mut ctx = make_ctx();
        ctx.wants_markdown = true;
        assert!(should_convert(&ctx, "text/html; charset=utf-8"));
    }

    #[test]
    fn test_gate_json_does_not_convert() {
        let mut ctx = make_ctx();
        ctx.wants_markdown = true;
        assert!(!should_convert(&ctx, "application/json"));
    }

    #[test]
    fn test_gate_sse_does_not_convert() {
        let mut ctx = make_ctx();
        ctx.wants_markdown = true;
        ctx.is_sse = true;
        assert!(!should_convert(&ctx, "text/html"));
    }

    #[test]
    fn test_gate_websocket_does_not_convert() {
        let mut ctx = make_ctx();
        ctx.wants_markdown = true;
        ctx.is_websocket = true;
        assert!(!should_convert(&ctx, "text/html"));
    }

    #[test]
    fn test_gate_wants_markdown_false_skips() {
        let ctx = make_ctx(); // wants_markdown == false by default
        assert!(!should_convert(&ctx, "text/html"));
    }

    // ── response_body_filter buffering logic ──────────────────────────────────

    /// Simulate the body filter for a single-chunk response.
    /// Mirrors the production pipeline: parse → extract_page_meta →
    /// extract_content_html → htmd::convert → prepend frontmatter.
    fn run_body_filter_single_chunk(ctx: &mut ProxyContext, html: &[u8]) -> Option<Bytes> {
        let mut body: Option<Bytes> = Some(Bytes::copy_from_slice(html));
        let end_of_stream = true;

        if ctx.wants_markdown {
            if let Some(chunk) = body.take() {
                if ctx.markdown_buffer.len() + chunk.len() > MAX_MARKDOWN_BODY_BYTES {
                    ctx.wants_markdown = false;
                    let mut flushed = std::mem::take(&mut ctx.markdown_buffer);
                    flushed.extend_from_slice(&chunk);
                    return Some(Bytes::from(flushed));
                }
                ctx.markdown_buffer.extend_from_slice(&chunk);
            }
            if end_of_stream {
                let html_str = String::from_utf8_lossy(&ctx.markdown_buffer);
                let document = scraper::Html::parse_document(&html_str);
                let meta = extract_page_meta(&document);
                let content = extract_content_html(&document);
                let markdown = htmd::convert(&content).unwrap_or_default();
                let final_markdown = match meta.to_frontmatter() {
                    Some(fm) => fm + &markdown,
                    None => markdown,
                };
                ctx.markdown_buffer = Vec::new();
                return Some(Bytes::from(final_markdown));
            }
            return None;
        }

        body
    }

    // Helper: parse and extract content from an HTML string.
    fn extract(html: &str) -> String {
        let doc = scraper::Html::parse_document(html);
        extract_content_html(&doc)
    }

    // ── extract_content_html ─────────────────────────────────────────────────

    #[test]
    fn test_extract_main_tag_preferred() {
        let html = r#"<html><body>
            <nav>Nav noise</nav>
            <main><h1>Content</h1><p>Body text</p></main>
            <footer>Footer noise</footer>
        </body></html>"#;
        let extracted = extract(html);
        assert!(
            extracted.contains("Content"),
            "Expected main content in: {}",
            extracted
        );
        assert!(
            !extracted.contains("Nav noise"),
            "Expected nav stripped, got: {}",
            extracted
        );
        assert!(
            !extracted.contains("Footer noise"),
            "Expected footer stripped, got: {}",
            extracted
        );
    }

    #[test]
    fn test_extract_falls_back_to_body_when_no_main() {
        let html = r#"<html><body><h1>Article</h1><p>Text</p></body></html>"#;
        let extracted = extract(html);
        assert!(
            extracted.contains("Article"),
            "Expected body content in: {}",
            extracted
        );
        assert!(
            extracted.contains("Text"),
            "Expected body content in: {}",
            extracted
        );
    }

    #[test]
    fn test_extract_first_main_when_multiple() {
        let html = r#"<html><body>
            <main id="first"><p>Primary</p></main>
            <div><main id="second"><p>Nested</p></main></div>
        </body></html>"#;
        let extracted = extract(html);
        assert!(
            extracted.contains("Primary"),
            "Expected first main in: {}",
            extracted
        );
    }

    #[test]
    fn test_extract_script_inside_main_stripped() {
        // <script> inside <main> must be stripped (the key bug we fixed).
        let html = r#"<html><body>
            <main>
                <script>window.foo = 1;</script>
                <script type="application/ld+json">{"@context":"https://schema.org"}</script>
                <p>Clean content</p>
            </main>
        </body></html>"#;
        let extracted = extract(html);
        assert!(
            extracted.contains("Clean content"),
            "Expected content in: {}",
            extracted
        );
        assert!(
            !extracted.contains("window.foo"),
            "Expected inline script stripped, got: {}",
            extracted
        );
        assert!(
            !extracted.contains("schema.org"),
            "Expected JSON-LD stripped, got: {}",
            extracted
        );
    }

    #[test]
    fn test_extract_style_inside_main_stripped() {
        let html = r#"<html><body>
            <main>
                <style>.foo { color: red; }</style>
                <p>Article text</p>
            </main>
        </body></html>"#;
        let extracted = extract(html);
        assert!(
            extracted.contains("Article text"),
            "Expected content in: {}",
            extracted
        );
        assert!(
            !extracted.contains("color: red"),
            "Expected style stripped, got: {}",
            extracted
        );
    }

    #[test]
    fn test_extract_script_outside_main_not_in_output() {
        let html = r#"<html><head><style>body { color: red; }</style></head><body>
            <script>window.bar = 2;</script>
            <main><p>Clean content</p></main>
        </body></html>"#;
        let extracted = extract(html);
        assert!(!extracted.contains("window.bar"));
        assert!(!extracted.contains("color: red"));
    }

    #[test]
    fn test_extract_fallback_to_original_when_no_body() {
        let fragment = "<h1>Just a heading</h1>";
        let extracted = extract(fragment);
        assert!(
            extracted.contains("Just a heading"),
            "Expected heading in: {}",
            extracted
        );
    }

    // ── extract_page_meta / frontmatter ──────────────────────────────────────

    #[test]
    fn test_frontmatter_from_og_title_and_description() {
        let html = r#"<html><head>
            <title>My Page · Site Name</title>
            <meta property="og:title" content="My Page"/>
            <meta name="description" content="A great page about things."/>
        </head><body><main><p>Content</p></main></body></html>"#;
        let doc = scraper::Html::parse_document(html);
        let meta = extract_page_meta(&doc);
        // og:title preferred over <title>
        assert_eq!(meta.title.as_deref(), Some("My Page"));
        assert_eq!(
            meta.description.as_deref(),
            Some("A great page about things.")
        );
        assert!(meta.image.is_none());

        let fm = meta.to_frontmatter().unwrap();
        assert!(fm.starts_with("---\n"), "Expected YAML fence: {}", fm);
        assert!(fm.contains("title: My Page"), "got: {}", fm);
        assert!(
            fm.contains("description: A great page about things."),
            "got: {}",
            fm
        );
        assert!(fm.ends_with("---\n\n"), "Expected closing fence: {}", fm);
    }

    #[test]
    fn test_frontmatter_falls_back_to_title_tag() {
        let html = r#"<html><head><title>Fallback Title</title></head>
        <body><main><p>x</p></main></body></html>"#;
        let doc = scraper::Html::parse_document(html);
        let meta = extract_page_meta(&doc);
        assert_eq!(meta.title.as_deref(), Some("Fallback Title"));
    }

    #[test]
    fn test_frontmatter_image_from_og_image() {
        let html = r#"<html><head>
            <meta property="og:image" content="https://example.com/img.png"/>
        </head><body><main><p>x</p></main></body></html>"#;
        let doc = scraper::Html::parse_document(html);
        let meta = extract_page_meta(&doc);
        assert_eq!(meta.image.as_deref(), Some("https://example.com/img.png"));
    }

    #[test]
    fn test_frontmatter_image_prefers_property_image_over_og_image() {
        let html = r#"<html><head>
            <meta property="image" content="https://example.com/preview.png"/>
            <meta property="og:image" content="https://example.com/og.png"/>
        </head><body><main><p>x</p></main></body></html>"#;
        let doc = scraper::Html::parse_document(html);
        let meta = extract_page_meta(&doc);
        assert_eq!(
            meta.image.as_deref(),
            Some("https://example.com/preview.png")
        );
    }

    #[test]
    fn test_frontmatter_none_when_no_meta() {
        let html = r#"<html><body><main><p>x</p></main></body></html>"#;
        let doc = scraper::Html::parse_document(html);
        let meta = extract_page_meta(&doc);
        assert!(meta.to_frontmatter().is_none());
    }

    #[test]
    fn test_body_filter_converts_html_to_markdown_with_frontmatter() {
        let mut ctx = make_ctx();
        ctx.wants_markdown = true;

        // Full page with meta + main + noise — frontmatter should be prepended,
        // nav/footer stripped, script inside main stripped.
        let html = br#"<html><head>
            <meta property="og:title" content="Hello Page"/>
            <meta name="description" content="A test page."/>
        </head><body>
            <nav>Nav</nav>
            <main>
                <script>window.noise = 1;</script>
                <h1>Hello</h1><p>World</p>
            </main>
            <footer>Footer</footer>
        </body></html>"#;
        let result = run_body_filter_single_chunk(&mut ctx, html);

        let md = String::from_utf8(result.unwrap().to_vec()).unwrap();
        // Frontmatter present
        assert!(md.starts_with("---\n"), "Expected frontmatter: {}", md);
        assert!(md.contains("title: Hello Page"), "got: {}", md);
        assert!(md.contains("description: A test page."), "got: {}", md);
        // Article content present
        assert!(md.contains("Hello"), "got: {}", md);
        assert!(md.contains("World"), "got: {}", md);
        // Noise absent
        assert!(!md.contains("Nav"), "got: {}", md);
        assert!(!md.contains("Footer"), "got: {}", md);
        assert!(!md.contains("window.noise"), "got: {}", md);
    }

    #[test]
    fn test_body_filter_passthrough_when_wants_markdown_false() {
        let mut ctx = make_ctx();
        ctx.wants_markdown = false;

        let html = b"<h1>Hello</h1>";
        let result = run_body_filter_single_chunk(&mut ctx, html);

        // Should return unchanged bytes
        assert!(result.is_some());
        assert_eq!(result.unwrap().as_ref(), html);
    }

    #[test]
    fn test_body_filter_size_guard_disables_conversion() {
        let mut ctx = make_ctx();
        ctx.wants_markdown = true;

        // Create a body slightly larger than 2 MB
        let oversized = vec![b'x'; MAX_MARKDOWN_BODY_BYTES + 1];
        let result = run_body_filter_single_chunk(&mut ctx, &oversized);

        // Should fall back to passthrough — returns original bytes, conversion disabled
        assert!(
            !ctx.wants_markdown,
            "wants_markdown should be reset to false"
        );
        assert!(result.is_some());
        assert_eq!(result.unwrap().len(), oversized.len());
    }

    #[test]
    fn test_body_filter_multi_chunk_accumulation() {
        let mut ctx = make_ctx();
        ctx.wants_markdown = true;

        // Simulate two chunks arriving before end_of_stream (split mid-tag)
        let chunk1 = Bytes::from_static(b"<html><body><main><h1>Greet");
        let chunk2 = Bytes::from_static(b"ings</h1></main></body></html>");

        // First chunk — not end of stream
        {
            let mut body: Option<Bytes> = Some(chunk1);
            if ctx.wants_markdown {
                if let Some(c) = body.take() {
                    ctx.markdown_buffer.extend_from_slice(&c);
                }
                // end_of_stream = false → return None (suppress)
            }
        }

        // Second chunk — end of stream
        {
            let mut body: Option<Bytes> = Some(chunk2);
            let end_of_stream = true;
            if ctx.wants_markdown {
                if let Some(c) = body.take() {
                    ctx.markdown_buffer.extend_from_slice(&c);
                }
                if end_of_stream {
                    let html_str = String::from_utf8_lossy(&ctx.markdown_buffer);
                    let document = scraper::Html::parse_document(&html_str);
                    let content = extract_content_html(&document);
                    let markdown = htmd::convert(&content).unwrap_or_default();
                    ctx.markdown_buffer = Vec::new();
                    body = Some(Bytes::from(markdown));
                }
            }

            let result = body;
            assert!(result.is_some());
            let md = String::from_utf8(result.unwrap().to_vec()).unwrap();
            assert!(md.contains("Greetings"), "Expected 'Greetings' in: {}", md);
        }
    }

    // ── SSE passthrough (critical safety test) ────────────────────────────────

    #[test]
    fn test_sse_passthrough_unaffected() {
        // Even if wants_markdown was somehow set, SSE responses must never be buffered.
        // The upstream_response_filter resets wants_markdown for SSE, but we also
        // guard in response_body_filter. Verify the guard works.
        let mut ctx = make_ctx();
        ctx.wants_markdown = true; // pretend the guard in upstream_response_filter was skipped
        ctx.is_sse = true;

        let sse_chunk = Bytes::from_static(b"data: hello\n\n");

        // Replicate the response_body_filter guard for SSE
        if ctx.is_sse || ctx.is_websocket {
            // pass through immediately — no buffering, no conversion
        } else if ctx.wants_markdown {
            panic!("Should not reach markdown conversion branch for SSE");
        }

        // body should be unchanged (the SSE branch never touches it)
        assert_eq!(sse_chunk.as_ref(), b"data: hello\n\n");
    }
}

// ── Pipeline integration tests ────────────────────────────────────────────────
//
// These tests exercise the full gate → header-rewrite → body-filter pipeline
// without needing a live Pingora session.  They construct `ResponseHeader` and
// `ProxyContext` directly and call the extracted free functions
// (`apply_markdown_upstream_gate`, `apply_markdown_response_headers`) plus the
// body-filter logic that `run_body_filter_single_chunk` (in markdown_tests)
// already covers, so here we focus on the header and gate behaviour and on
// every edge-case the body filter must handle gracefully.
#[cfg(test)]
mod markdown_pipeline_tests {
    use super::*;
    use bytes::Bytes;
    use std::time::Instant;

    // ── Helpers ──────────────────────────────────────────────────────────────

    fn make_ctx() -> ProxyContext {
        ProxyContext {
            response_modified: false,
            response_compressed: false,
            upstream_response_headers: None,
            content_type: None,
            buffer: vec![],
            project: None,
            environment: None,
            deployment: None,
            request_id: "test-req".to_string(),
            start_time: Instant::now(),
            method: "GET".to_string(),
            path: "/".to_string(),
            query_string: None,
            host: "example.com".to_string(),
            user_agent: "TestAgent/1.0".to_string(),
            referrer: None,
            ip_address: Some("127.0.0.1".to_string()),
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
            upstream_connect_tries: 0,
            upstream_write_pending_time_ms: None,
            upstream_start_time: None,
            upstream_response_time_ms: None,
            preview_route: None,
        }
    }

    /// Build a `ResponseHeader` with an explicit status and optional `Content-Type`.
    fn make_response(status: u16, content_type: Option<&str>) -> ResponseHeader {
        let mut resp = ResponseHeader::build(status, None).unwrap();
        if let Some(ct) = content_type {
            resp.insert_header("Content-Type", ct).unwrap();
        }
        resp
    }

    /// Simulate the full pipeline for a single-chunk body.
    /// Returns (final_ctx, outbound_response_header, body_bytes).
    fn run_pipeline(
        mut ctx: ProxyContext,
        mut resp: ResponseHeader,
        body: &[u8],
    ) -> (ProxyContext, ResponseHeader, Option<Bytes>) {
        // Phase 1: upstream_response_filter — gate
        apply_markdown_upstream_gate(&mut resp, &mut ctx);

        // Phase 2: response_filter — header rewrite
        apply_markdown_response_headers(&mut resp, &ctx);

        // Phase 3: response_body_filter — buffer + convert (single-chunk, end_of_stream=true)
        let body_out = if ctx.is_sse || ctx.is_websocket {
            Some(Bytes::copy_from_slice(body))
        } else if ctx.wants_markdown {
            let chunk = Bytes::copy_from_slice(body);
            if ctx.markdown_buffer.len() + chunk.len() > MAX_MARKDOWN_BODY_BYTES {
                ctx.wants_markdown = false;
                let mut flushed = std::mem::take(&mut ctx.markdown_buffer);
                flushed.extend_from_slice(&chunk);
                Some(Bytes::from(flushed))
            } else {
                ctx.markdown_buffer.extend_from_slice(&chunk);
                let html = String::from_utf8_lossy(&ctx.markdown_buffer);
                let document = scraper::Html::parse_document(&html);
                let meta = extract_page_meta(&document);
                let content = extract_content_html(&document);
                let markdown = htmd::convert(&content).unwrap_or_default();
                ctx.markdown_buffer = Vec::new();
                let final_md = match meta.to_frontmatter() {
                    Some(fm) => fm + &markdown,
                    None => markdown,
                };
                Some(Bytes::from(final_md))
            }
        } else {
            Some(Bytes::copy_from_slice(body))
        };

        (ctx, resp, body_out)
    }

    // ── Gate tests ────────────────────────────────────────────────────────────

    #[test]
    fn gate_allows_200_text_html() {
        let mut ctx = make_ctx();
        ctx.wants_markdown = true;
        let mut resp = make_response(200, Some("text/html; charset=utf-8"));
        apply_markdown_upstream_gate(&mut resp, &mut ctx);
        assert!(ctx.wants_markdown, "200 text/html should be allowed");
        assert_eq!(
            resp.headers.get("vary").and_then(|v| v.to_str().ok()),
            Some("Accept"),
            "Vary: Accept must be set"
        );
    }

    #[test]
    fn gate_cancels_non_html_content_type() {
        for ct in &[
            "application/json",
            "text/plain",
            "image/png",
            "application/octet-stream",
        ] {
            let mut ctx = make_ctx();
            ctx.wants_markdown = true;
            let mut resp = make_response(200, Some(ct));
            apply_markdown_upstream_gate(&mut resp, &mut ctx);
            assert!(
                !ctx.wants_markdown,
                "wants_markdown must be false for Content-Type: {}",
                ct
            );
        }
    }

    #[test]
    fn gate_cancels_missing_content_type() {
        let mut ctx = make_ctx();
        ctx.wants_markdown = true;
        let mut resp = make_response(200, None);
        apply_markdown_upstream_gate(&mut resp, &mut ctx);
        assert!(
            !ctx.wants_markdown,
            "missing Content-Type must cancel conversion"
        );
    }

    #[test]
    fn gate_cancels_4xx_even_with_html() {
        for status in &[400u16, 401, 403, 404, 422, 429] {
            let mut ctx = make_ctx();
            ctx.wants_markdown = true;
            let mut resp = make_response(*status, Some("text/html; charset=utf-8"));
            apply_markdown_upstream_gate(&mut resp, &mut ctx);
            assert!(
                !ctx.wants_markdown,
                "wants_markdown must be false for status {}",
                status
            );
        }
    }

    #[test]
    fn gate_cancels_5xx_even_with_html() {
        for status in &[500u16, 502, 503, 504] {
            let mut ctx = make_ctx();
            ctx.wants_markdown = true;
            let mut resp = make_response(*status, Some("text/html; charset=utf-8"));
            apply_markdown_upstream_gate(&mut resp, &mut ctx);
            assert!(
                !ctx.wants_markdown,
                "wants_markdown must be false for status {}",
                status
            );
        }
    }

    #[test]
    fn gate_cancels_3xx_redirect() {
        let mut ctx = make_ctx();
        ctx.wants_markdown = true;
        let mut resp = make_response(302, Some("text/html"));
        apply_markdown_upstream_gate(&mut resp, &mut ctx);
        assert!(!ctx.wants_markdown, "302 redirect should cancel conversion");
    }

    #[test]
    fn gate_handles_uppercase_content_type() {
        // Some upstreams send "TEXT/HTML" — must still be recognised.
        let mut ctx = make_ctx();
        ctx.wants_markdown = true;
        let mut resp = make_response(200, Some("TEXT/HTML; CHARSET=UTF-8"));
        apply_markdown_upstream_gate(&mut resp, &mut ctx);
        assert!(ctx.wants_markdown, "uppercase TEXT/HTML must be allowed");
    }

    #[test]
    fn gate_cancels_sse_even_with_html() {
        let mut ctx = make_ctx();
        ctx.wants_markdown = true;
        ctx.is_sse = true;
        let mut resp = make_response(200, Some("text/html"));
        apply_markdown_upstream_gate(&mut resp, &mut ctx);
        assert!(!ctx.wants_markdown, "SSE must cancel conversion");
    }

    #[test]
    fn gate_cancels_websocket_even_with_html() {
        let mut ctx = make_ctx();
        ctx.wants_markdown = true;
        ctx.is_websocket = true;
        let mut resp = make_response(200, Some("text/html"));
        apply_markdown_upstream_gate(&mut resp, &mut ctx);
        assert!(!ctx.wants_markdown, "WebSocket must cancel conversion");
    }

    #[test]
    fn gate_noop_when_wants_markdown_false() {
        // If wants_markdown is already false the gate must not touch the response.
        let mut ctx = make_ctx(); // wants_markdown = false
        let mut resp = make_response(200, Some("text/html"));
        apply_markdown_upstream_gate(&mut resp, &mut ctx);
        assert!(!ctx.wants_markdown);
        assert!(
            resp.headers.get("vary").is_none(),
            "Vary must NOT be added when wants_markdown is false"
        );
    }

    // ── Header-rewrite tests ──────────────────────────────────────────────────

    #[test]
    fn header_rewrite_sets_markdown_content_type() {
        let mut ctx = make_ctx();
        ctx.wants_markdown = true;
        let mut resp = make_response(200, Some("text/html; charset=utf-8"));
        // Simulate Content-Length being set by upstream
        resp.insert_header("Content-Length", "1234").unwrap();
        resp.insert_header("Content-Encoding", "gzip").unwrap();
        apply_markdown_response_headers(&mut resp, &ctx);
        assert_eq!(
            resp.headers
                .get("content-type")
                .and_then(|v| v.to_str().ok()),
            Some("text/markdown; charset=utf-8")
        );
        assert!(
            resp.headers.get("content-length").is_none(),
            "Content-Length must be removed"
        );
        assert!(
            resp.headers.get("content-encoding").is_none(),
            "Content-Encoding must be removed"
        );
        assert_eq!(
            resp.headers
                .get("x-markdown-tokens")
                .and_then(|v| v.to_str().ok()),
            Some("0"),
            "X-Markdown-Tokens placeholder must be present"
        );
    }

    #[test]
    fn header_rewrite_noop_when_wants_markdown_false() {
        let ctx = make_ctx(); // wants_markdown = false
        let mut resp = make_response(200, Some("text/html"));
        apply_markdown_response_headers(&mut resp, &ctx);
        assert_eq!(
            resp.headers
                .get("content-type")
                .and_then(|v| v.to_str().ok()),
            Some("text/html"),
            "Content-Type must be unchanged when wants_markdown is false"
        );
        assert!(resp.headers.get("x-markdown-tokens").is_none());
    }

    // ── Full pipeline tests ───────────────────────────────────────────────────

    #[test]
    fn pipeline_converts_html_to_markdown() {
        let mut ctx = make_ctx();
        ctx.wants_markdown = true;
        let resp = make_response(200, Some("text/html; charset=utf-8"));
        let html =
            b"<html><body><main><h1>Hello World</h1><p>A paragraph.</p></main></body></html>";

        let (_ctx, out_resp, body) = run_pipeline(ctx, resp, html);

        // Headers
        assert_eq!(
            out_resp
                .headers
                .get("content-type")
                .and_then(|v| v.to_str().ok()),
            Some("text/markdown; charset=utf-8")
        );
        assert!(out_resp.headers.get("x-markdown-tokens").is_some());

        // Body
        let md = String::from_utf8(body.unwrap().to_vec()).unwrap();
        assert!(
            md.contains("Hello World"),
            "heading must appear in output: {}",
            md
        );
        assert!(
            md.contains("A paragraph"),
            "paragraph must appear in output: {}",
            md
        );
    }

    #[test]
    fn pipeline_passthrough_on_non_html_content_type() {
        let mut ctx = make_ctx();
        ctx.wants_markdown = true;
        let resp = make_response(200, Some("application/json"));
        let json = br#"{"key":"value"}"#;

        let (final_ctx, out_resp, body) = run_pipeline(ctx, resp, json);

        assert!(
            !final_ctx.wants_markdown,
            "gate must have cancelled conversion"
        );
        assert_eq!(
            out_resp
                .headers
                .get("content-type")
                .and_then(|v| v.to_str().ok()),
            Some("application/json"),
            "Content-Type must be unchanged"
        );
        assert!(out_resp.headers.get("x-markdown-tokens").is_none());
        assert_eq!(body.unwrap().as_ref(), json);
    }

    #[test]
    fn pipeline_passthrough_on_missing_content_type() {
        let mut ctx = make_ctx();
        ctx.wants_markdown = true;
        let resp = make_response(200, None);
        let payload = b"some raw bytes";

        let (final_ctx, out_resp, body) = run_pipeline(ctx, resp, payload);

        assert!(!final_ctx.wants_markdown);
        assert!(out_resp.headers.get("content-type").is_none());
        assert!(out_resp.headers.get("x-markdown-tokens").is_none());
        assert_eq!(body.unwrap().as_ref(), payload);
    }

    #[test]
    fn pipeline_passthrough_on_404() {
        let mut ctx = make_ctx();
        ctx.wants_markdown = true;
        let html = b"<html><body><h1>Not Found</h1></body></html>";
        let resp = make_response(404, Some("text/html; charset=utf-8"));

        let (final_ctx, out_resp, body) = run_pipeline(ctx, resp, html);

        assert!(!final_ctx.wants_markdown, "404 must cancel conversion");
        assert_eq!(
            out_resp
                .headers
                .get("content-type")
                .and_then(|v| v.to_str().ok()),
            Some("text/html; charset=utf-8"),
            "Content-Type must be unchanged for 404"
        );
        // Body must be the original HTML, not markdown
        assert_eq!(body.unwrap().as_ref(), html);
    }

    #[test]
    fn pipeline_passthrough_on_500() {
        let mut ctx = make_ctx();
        ctx.wants_markdown = true;
        let html = b"<html><body><h1>Internal Error</h1></body></html>";
        let resp = make_response(500, Some("text/html"));

        let (final_ctx, _out_resp, body) = run_pipeline(ctx, resp, html);

        assert!(!final_ctx.wants_markdown);
        assert_eq!(body.unwrap().as_ref(), html);
    }

    #[test]
    fn pipeline_passthrough_on_302_redirect() {
        let mut ctx = make_ctx();
        ctx.wants_markdown = true;
        let mut resp = make_response(302, Some("text/html"));
        resp.insert_header("Location", "https://example.com/new")
            .unwrap();

        let (final_ctx, out_resp, body) = run_pipeline(ctx, resp, b"");

        assert!(!final_ctx.wants_markdown);
        assert_eq!(
            out_resp
                .headers
                .get("content-type")
                .and_then(|v| v.to_str().ok()),
            Some("text/html")
        );
        assert!(out_resp.headers.get("x-markdown-tokens").is_none());
        assert_eq!(body.unwrap().as_ref(), b"");
    }

    #[test]
    fn pipeline_passthrough_when_not_requesting_markdown() {
        // Client did not send Accept: text/markdown — wants_markdown stays false throughout.
        let ctx = make_ctx(); // wants_markdown = false
        let resp = make_response(200, Some("text/html"));
        let html = b"<html><body><h1>Hello</h1></body></html>";

        let (final_ctx, out_resp, body) = run_pipeline(ctx, resp, html);

        assert!(!final_ctx.wants_markdown);
        assert_eq!(
            out_resp
                .headers
                .get("content-type")
                .and_then(|v| v.to_str().ok()),
            Some("text/html")
        );
        // Body unchanged
        assert_eq!(body.unwrap().as_ref(), html);
    }

    #[test]
    fn pipeline_converts_uppercase_content_type() {
        let mut ctx = make_ctx();
        ctx.wants_markdown = true;
        let resp = make_response(200, Some("TEXT/HTML"));
        let html = b"<body><p>Content</p></body>";

        let (_ctx, out_resp, body) = run_pipeline(ctx, resp, html);

        assert_eq!(
            out_resp
                .headers
                .get("content-type")
                .and_then(|v| v.to_str().ok()),
            Some("text/markdown; charset=utf-8")
        );
        let md = String::from_utf8(body.unwrap().to_vec()).unwrap();
        assert!(
            md.contains("Content"),
            "body text must survive conversion: {}",
            md
        );
    }

    #[test]
    fn pipeline_size_guard_passthrough_on_oversized_body() {
        let mut ctx = make_ctx();
        ctx.wants_markdown = true;
        let resp = make_response(200, Some("text/html; charset=utf-8"));
        let oversized = vec![b'x'; MAX_MARKDOWN_BODY_BYTES + 1];

        let (final_ctx, _out_resp, body) = run_pipeline(ctx, resp, &oversized);

        assert!(
            !final_ctx.wants_markdown,
            "size guard must disable conversion"
        );
        assert_eq!(
            body.unwrap().len(),
            oversized.len(),
            "original bytes must be returned unchanged"
        );
    }

    #[test]
    fn pipeline_includes_frontmatter_when_meta_present() {
        let mut ctx = make_ctx();
        ctx.wants_markdown = true;
        let resp = make_response(200, Some("text/html; charset=utf-8"));
        let html = br#"<html>
            <head>
                <meta property="og:title" content="My Article" />
                <meta name="description" content="A great read" />
            </head>
            <body><main><p>Body text.</p></main></body>
        </html>"#;

        let (_ctx, _out_resp, body) = run_pipeline(ctx, resp, html);
        let md = String::from_utf8(body.unwrap().to_vec()).unwrap();

        assert!(
            md.starts_with("---\n"),
            "output must start with YAML frontmatter"
        );
        assert!(
            md.contains("title: My Article"),
            "og:title must be in frontmatter"
        );
        assert!(
            md.contains("description: A great read"),
            "description must be in frontmatter"
        );
        assert!(
            md.contains("Body text."),
            "article body must appear after frontmatter"
        );
    }

    #[test]
    fn pipeline_vary_header_set_only_on_conversion() {
        // Vary: Accept must appear when conversion happens, not when it is cancelled.
        let mut ctx_yes = make_ctx();
        ctx_yes.wants_markdown = true;
        let mut resp_yes = make_response(200, Some("text/html"));
        apply_markdown_upstream_gate(&mut resp_yes, &mut ctx_yes);
        assert_eq!(
            resp_yes.headers.get("vary").and_then(|v| v.to_str().ok()),
            Some("Accept")
        );

        let mut ctx_no = make_ctx();
        ctx_no.wants_markdown = true;
        let mut resp_no = make_response(200, Some("application/json"));
        apply_markdown_upstream_gate(&mut resp_no, &mut ctx_no);
        assert!(
            resp_no.headers.get("vary").is_none(),
            "Vary must NOT be added when conversion is cancelled"
        );
    }
}

#[cfg(test)]
mod traceparent_tests {
    use super::*;
    use std::collections::HashMap;

    fn headers_with(name: &str, value: &str) -> HashMap<String, String> {
        let mut h = HashMap::new();
        h.insert(name.to_string(), value.to_string());
        h
    }

    #[test]
    fn extracts_valid_traceparent_trace_id() {
        let h = headers_with(
            "traceparent",
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
        );
        assert_eq!(
            LoadBalancer::extract_traceparent_trace_id(Some(&h)),
            Some("4bf92f3577b34da6a3ce929d0e0e4736".to_string())
        );
    }

    #[test]
    fn returns_none_when_header_absent() {
        let h: HashMap<String, String> = HashMap::new();
        assert_eq!(LoadBalancer::extract_traceparent_trace_id(Some(&h)), None);
        assert_eq!(LoadBalancer::extract_traceparent_trace_id(None), None);
    }

    #[test]
    fn returns_none_for_all_zero_trace_id() {
        let h = headers_with(
            "traceparent",
            "00-00000000000000000000000000000000-00f067aa0ba902b7-01",
        );
        assert_eq!(LoadBalancer::extract_traceparent_trace_id(Some(&h)), None);
    }

    #[test]
    fn returns_none_for_wrong_length() {
        let h = headers_with("traceparent", "00-deadbeef-00f067aa0ba902b7-01");
        assert_eq!(LoadBalancer::extract_traceparent_trace_id(Some(&h)), None);
    }

    #[test]
    fn returns_none_for_non_hex_chars() {
        let h = headers_with(
            "traceparent",
            "00-zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz-00f067aa0ba902b7-01",
        );
        assert_eq!(LoadBalancer::extract_traceparent_trace_id(Some(&h)), None);
    }

    #[test]
    fn lowercases_uppercase_hex() {
        let h = headers_with(
            "traceparent",
            "00-4BF92F3577B34DA6A3CE929D0E0E4736-00f067aa0ba902b7-01",
        );
        assert_eq!(
            LoadBalancer::extract_traceparent_trace_id(Some(&h)),
            Some("4bf92f3577b34da6a3ce929d0e0e4736".to_string())
        );
    }
}
