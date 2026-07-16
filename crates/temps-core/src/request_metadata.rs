use axum::extract::Request;
use axum::http::HeaderMap;
use axum::middleware::Next;
use axum::response::Response;
use cookie::Cookie;
use std::sync::Arc;

use crate::cookie_crypto::CookieCrypto;

#[derive(Clone)]
pub struct RequestMetadata {
    pub ip_address: String,
    pub user_agent: String,
    pub headers: HeaderMap,
    pub visitor_id_cookie: Option<String>,
    pub session_id_cookie: Option<String>,
    /// Full origin including scheme and `:port` suffix when present.
    /// Suitable for constructing user-visible absolute URLs (links, redirects).
    pub base_url: String,
    pub scheme: String, // "http" or "https"
    /// Hostname from the Host header with any `:port` suffix stripped.
    /// Matches the key used by the proxy route table, so handlers can safely
    /// pass this into `CachedPeerTable::get_route(&metadata.host)` without
    /// worrying about non-standard ports (e.g. the :8080 dev proxy).
    pub host: String,
    pub is_secure: bool, // true if HTTPS
}

const SESSION_ID_COOKIE_NAME: &str = "_temps_sid";
const VISITOR_ID_COOKIE_NAME: &str = "_temps_visitor_id";

/// Strip any `:port` suffix from a raw Host header.
///
/// The proxy's route table is keyed on the hostname only, so requests that
/// arrive on non-default ports (dev setups, `localho.st:8080`, etc.) must be
/// normalized before lookup. IPv6 literals are not supported in Host headers
/// without brackets, so naive `split(':')` is sufficient here.
pub fn host_without_port(raw_host: &str) -> &str {
    raw_host.split(':').next().unwrap_or(raw_host)
}

fn extract_encrypted_cookie(
    headers: &HeaderMap,
    name: &str,
    crypto: &CookieCrypto,
) -> Option<String> {
    for cookie_header in headers.get_all("Cookie") {
        if let Ok(cookie_str) = cookie_header.to_str() {
            for cookie in Cookie::split_parse(cookie_str).flatten() {
                if cookie.name() == name {
                    return crypto.decrypt(cookie.value()).ok();
                }
            }
        }
    }
    None
}

/// Normalise a decrypted session cookie value to a bare UUID.
///
/// The proxy writes session cookies as a v2 payload (`v2|<uuid>|<unix_secs>`)
/// so it can check session freshness in-process without a database round-trip.
/// The proxy's own `parse_session_cookie` (in `temps-proxy::cookie_codec`)
/// already strips the prefix before storing `request_sessions.session_id` as
/// a bare UUID.  The analytics ingest path previously called only
/// `extract_encrypted_cookie`, which returned the full raw plaintext including
/// the prefix — making every `JOIN request_sessions rs ON rs.session_id =
/// e.session_id` a permanent miss (`'v2|uuid|ts' ≠ 'uuid'`).
///
/// This function mirrors the same parsing rule:
/// - `v2|<uuid>|<ts>` → returns `Some(uuid)` (extracted middle segment)
/// - bare UUID (legacy cookies) → returns `Some(plaintext)` unchanged
/// - malformed v2 (missing uuid segment) → returns `None` (proxy rejects these too)
pub fn normalize_session_cookie(plaintext: String) -> Option<String> {
    if !plaintext.starts_with("v2|") {
        // Legacy bare-UUID cookie or unknown format — accept as-is.
        return Some(plaintext);
    }
    // Format: "v2|<uuid>|<ts_secs>"
    let mut parts = plaintext.splitn(3, '|');
    let _prefix = parts.next(); // "v2"
    match parts.next() {
        Some(uuid) if !uuid.is_empty() => Some(uuid.to_string()),
        _ => None, // malformed v2: proxy rejects these too
    }
}

/// Build a `RequestMetadata` value from the incoming request. Used by the
/// middleware below, and also reusable from tests that synthesize requests.
pub fn build_from_request(req: &Request, crypto: &CookieCrypto) -> RequestMetadata {
    let headers = req.headers();

    // Resolve the client IP trust-awarely: `X-Forwarded-For` is only honored
    // from a loopback peer (our Pingora proxy), so a client connecting directly
    // cannot spoof it. The peer socket is injected by axum's connect-info make
    // service on every listener. Falls back to "unknown" when absent.
    let peer = req
        .extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|connect_info| connect_info.0);
    let ip_address = crate::resolve_client_ip(headers, peer);

    let visitor_id_cookie = extract_encrypted_cookie(headers, VISITOR_ID_COOKIE_NAME, crypto);
    // Decrypt the session cookie then normalise from the v2 payload format
    // (`v2|<uuid>|<unix_secs>`) to a bare UUID. The proxy's own
    // `parse_session_cookie` already does this normalisation before writing
    // `request_sessions.session_id`, so the analytics JOIN key and the
    // events column must agree.
    let session_id_cookie = extract_encrypted_cookie(headers, SESSION_ID_COOKIE_NAME, crypto)
        .and_then(normalize_session_cookie);

    let raw_host = headers
        .get("host")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("localhost")
        .to_string();

    let scheme = if headers
        .get("x-forwarded-proto")
        .and_then(|h| h.to_str().ok())
        == Some("https")
    {
        "https"
    } else {
        "http"
    };
    let is_secure = scheme == "https";
    let base_url = format!("{}://{}", scheme, raw_host);
    let host = host_without_port(&raw_host).to_string();

    RequestMetadata {
        ip_address,
        user_agent: headers
            .get("user-agent")
            .and_then(|h| h.to_str().ok())
            .unwrap_or("unknown")
            .to_string(),
        headers: headers.clone(),
        visitor_id_cookie,
        session_id_cookie,
        base_url,
        scheme: scheme.to_string(),
        host,
        is_secure,
    }
}

/// Middleware that constructs a `RequestMetadata` value from the incoming
/// request and inserts it into request extensions. Must run before any
/// handler that extracts `Extension<RequestMetadata>`.
///
/// Applied to both the admin and public routers in
/// `PluginManager::build_split_application` — public ingest endpoints
/// (session replay init, analytics events, etc.) depend on this metadata
/// even though they don't go through auth.
pub async fn request_metadata_middleware(
    crypto: Arc<CookieCrypto>,
    mut req: Request,
    next: Next,
) -> Response {
    let metadata = build_from_request(&req, crypto.as_ref());
    req.extensions_mut().insert(metadata);
    next.run(req).await
}

/// `TempsMiddleware` implementation for request-metadata injection. Owns an
/// `Arc<CookieCrypto>` so the plugin system can construct it once at startup
/// and reuse it across both the admin and public routers.
pub struct RequestMetadataMiddleware {
    crypto: Arc<CookieCrypto>,
}

impl RequestMetadataMiddleware {
    pub fn new(crypto: Arc<CookieCrypto>) -> Self {
        Self { crypto }
    }
}

impl crate::plugin::TempsMiddleware for RequestMetadataMiddleware {
    fn name(&self) -> &'static str {
        "request_metadata_middleware"
    }

    fn plugin_name(&self) -> &'static str {
        "core"
    }

    fn priority(&self) -> crate::plugin::MiddlewarePriority {
        // Runs before auth (Security=0) so auth handlers can also read the
        // metadata extension. Observability=100 is the natural slot for
        // request-context enrichment.
        crate::plugin::MiddlewarePriority::Observability
    }

    fn apply_to_public(&self) -> bool {
        true
    }

    fn execute<'a>(
        &'a self,
        mut req: Request,
        next: Next,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Response, axum::http::StatusCode>> + Send + 'a>,
    > {
        Box::pin(async move {
            let metadata = build_from_request(&req, self.crypto.as_ref());
            req.extensions_mut().insert(metadata);
            Ok(next.run(req).await)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request as HttpRequest, StatusCode};
    use axum::routing::get;
    use axum::{Extension, Router};
    use tower::ServiceExt;

    fn test_crypto() -> Arc<CookieCrypto> {
        let key = [42u8; 32];
        Arc::new(CookieCrypto::from_bytes(&key))
    }

    // ── normalize_session_cookie ─────────────────────────────────────────────

    #[test]
    fn normalize_session_cookie_v2_format_extracts_uuid() {
        let uuid = "c172f0b5-986f-47dc-b6c6-9198761519e0";
        let plaintext = format!("v2|{}|1783631315", uuid);
        assert_eq!(
            normalize_session_cookie(plaintext),
            Some(uuid.to_string()),
            "v2 payload must be normalised to bare UUID so it can JOIN request_sessions.session_id"
        );
    }

    #[test]
    fn normalize_session_cookie_bare_uuid_passthrough() {
        let uuid = "c172f0b5-986f-47dc-b6c6-9198761519e0".to_string();
        assert_eq!(
            normalize_session_cookie(uuid.clone()),
            Some(uuid),
            "legacy bare-UUID cookie must pass through unchanged"
        );
    }

    #[test]
    fn normalize_session_cookie_malformed_v2_returns_none() {
        // Missing UUID segment — proxy rejects this too
        assert_eq!(
            normalize_session_cookie("v2|".to_string()),
            None,
            "v2 prefix with empty UUID segment must return None"
        );
    }

    #[test]
    fn normalize_session_cookie_v2_missing_ts_still_extracts_uuid() {
        // v2 with UUID but no timestamp segment: we only need the UUID, ts is
        // not required for analytics purposes.
        let uuid = "c172f0b5-986f-47dc-b6c6-9198761519e0";
        // splitn(3, '|') on "v2|uuid" yields ["v2", "uuid"]: the third split
        // (ts) is absent, but we only care about the second (uuid).
        let plaintext = format!("v2|{}", uuid);
        assert_eq!(
            normalize_session_cookie(plaintext),
            Some(uuid.to_string()),
            "v2 without ts segment must still extract the UUID"
        );
    }

    // ── host_without_port ────────────────────────────────────────────────────

    #[test]
    fn strips_port_when_present() {
        assert_eq!(
            host_without_port("sandbox-test.localho.st:8080"),
            "sandbox-test.localho.st"
        );
    }

    #[test]
    fn passes_through_when_no_port() {
        assert_eq!(host_without_port("example.com"), "example.com");
    }

    #[test]
    fn handles_empty_string() {
        assert_eq!(host_without_port(""), "");
    }

    #[tokio::test]
    async fn middleware_injects_metadata_extension() {
        let crypto = test_crypto();
        let app = Router::new()
            .route(
                "/echo",
                get(|Extension(meta): Extension<RequestMetadata>| async move {
                    format!(
                        "host={};scheme={};ua={}",
                        meta.host, meta.scheme, meta.user_agent
                    )
                }),
            )
            .layer(axum::middleware::from_fn({
                let crypto = crypto.clone();
                move |req, next| {
                    let crypto = crypto.clone();
                    async move { request_metadata_middleware(crypto, req, next).await }
                }
            }));

        let response = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/echo")
                    .header("host", "example.com:8080")
                    .header("user-agent", "regression-test/1.0")
                    .header("x-forwarded-proto", "https")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap();
        let body_str = std::str::from_utf8(&body).unwrap();
        assert!(body_str.contains("host=example.com"));
        assert!(body_str.contains("scheme=https"));
        assert!(body_str.contains("ua=regression-test/1.0"));
    }

    /// `build_from_request` must derive `ip_address` trust-awarely from the
    /// connect-info peer, not from the raw leftmost `X-Forwarded-For` (which a
    /// direct client can spoof).
    #[test]
    fn build_from_request_resolves_trustworthy_client_ip() {
        use axum::extract::ConnectInfo;

        fn ip_of(peer: &str, xff: &str) -> String {
            let crypto = test_crypto();
            let mut req = HttpRequest::builder()
                .uri("/")
                .header("x-forwarded-for", xff)
                .body(Body::empty())
                .unwrap();
            req.extensions_mut()
                .insert(ConnectInfo(peer.parse::<std::net::SocketAddr>().unwrap()));
            build_from_request(&req, &crypto).ip_address
        }

        // Loopback proxy peer → the rightmost (proxy-appended) XFF entry wins;
        // the attacker-supplied leftmost value is ignored.
        assert_eq!(
            ip_of("127.0.0.1:9000", "6.6.6.6, 203.0.113.9"),
            "203.0.113.9"
        );

        // Direct, untrusted peer → the socket IP wins and XFF is ignored, so a
        // spoofed header cannot forge the audited IP.
        assert_eq!(ip_of("203.0.113.5:42424", "1.1.1.1"), "203.0.113.5");

        // No connect-info peer at all → "unknown" (never a spoofable header).
        let crypto = test_crypto();
        let req = HttpRequest::builder()
            .uri("/")
            .header("x-forwarded-for", "9.9.9.9")
            .body(Body::empty())
            .unwrap();
        assert_eq!(build_from_request(&req, &crypto).ip_address, "unknown");
    }

    #[tokio::test]
    async fn handler_without_middleware_fails_with_missing_extension() {
        // Regression guard: without the middleware, an
        // Extension<RequestMetadata> extractor on a route returns 500. This
        // pins the failure mode so we can rely on the middleware tests
        // above to catch the wiring break.
        let app = Router::new().route(
            "/echo",
            get(|Extension(_meta): Extension<RequestMetadata>| async move { "ok" }),
        );

        let response = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/echo")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }
}
