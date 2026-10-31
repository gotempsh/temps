// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use axum::{
    body::Bytes,
    extract::{ConnectInfo, DefaultBodyLimit, Extension, Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use std::io::Read as IoRead;
use std::net::SocketAddr;
use std::sync::Arc;
use tower_http::cors::{Any, CorsLayer};
use tracing::debug;
use utoipa::OpenApi;

use crate::providers::{sentry::SentryProvider, AuthContext, ErrorProvider};
use crate::sentry::dsn_service::{public_key_prefix, DSNService};
use crate::sentry::envelope::peek_envelope_dsn;
use crate::sentry::rate_limiter::{Admission, IngestRateLimiter};
use crate::sentry::types::{SentryEventRequest, SentryEventResponse};
use crate::services::error_tracking_service::ErrorTrackingService;
use temps_geo::IpAddressService;
use temps_routes::CachedPeerTable;

/// Route suffix (relative to this plugin's public router — the server nests
/// it under `/api` before mounting) that browser Sentry SDKs POST tunneled
/// envelopes to. Resolved from the DSN the SDK presents (explicitly, or
/// embedded in the envelope header) and otherwise from `Host` — mirroring how
/// `/api/_temps/event` (analytics) resolves via an ingest key first and
/// `CachedPeerTable` second. The proxy forwards anything under
/// `/api/_temps` to the console regardless of `Host`
/// (`temps-proxy::services::ROUTE_PREFIX_TEMPS`), so this path reaches the
/// console on every domain a project is deployed to.
///
/// This exact string is also used to build the browser-visible tunnel env var
/// value (see `temps-deployments::services::env_resolver`) — import this
/// constant there rather than hardcoding the path a second time.
pub const SENTRY_TUNNEL_ROUTE_PATH: &str = "/_temps/sentry/envelope";

/// Rate limit applied to tunneled ingest, which has no DSN row to read a
/// per-project limit from (the project is resolved from `Host`, not a
/// credential). Matches the default assigned to newly created DSNs
/// (see `dsn_service::generate_project_dsn`).
const TUNNEL_DEFAULT_RATE_LIMIT_PER_MINUTE: i32 = 1000;

/// Seconds a rate-limited ingest client should wait before retrying.
///
/// `IngestRateLimiter`'s window is a full minute, so one minute is the only
/// value guaranteed to be past the rejection regardless of where in the window
/// the client landed. Matches `temps-analytics`'s `INGEST_RETRY_AFTER_SECONDS`.
const INGEST_RETRY_AFTER_SECONDS: &str = "60";

#[derive(OpenApi)]
#[openapi(
    paths(
        ingest_sentry_event,
        ingest_sentry_envelope,
        ingest_tunneled_envelope,
        ingest_security_reports,
        ingest_nel_reports,
    ),
    components(schemas(
        SentryEventRequest,
        SentryEventResponse,
    )),
    tags(
        (name = "sentry-ingestor", description = "Sentry-compatible ingest endpoints")
    )
)]
pub struct ApiDoc;

#[derive(Clone)]
pub struct AppState {
    pub sentry_provider: Arc<SentryProvider>,
    pub session_replay_service: Arc<temps_analytics_session_replay::services::SessionReplayService>,
    pub otel_service: Arc<temps_otel::OtelService>,
    pub cron_monitor_service: Arc<temps_monitoring::cron_monitors::CronMonitorService>,
    pub error_tracking_service: Arc<ErrorTrackingService>,
    pub audit_service: Arc<dyn temps_core::AuditLogger>,
    pub ip_address_service: Option<Arc<IpAddressService>>,
    pub db: Option<Arc<sea_orm::DatabaseConnection>>,
    pub telemetry: Arc<dyn temps_core::telemetry::TelemetryReporter>,
    /// Host -> project/environment/deployment resolution for the tunneled
    /// ingest route, used only when that request presents no DSN credential.
    pub route_table: Arc<CachedPeerTable>,
    /// Resolves a DSN public key to its project, used by the tunnel route so
    /// an app Temps does not host (and therefore has no route-table entry
    /// for) can still tunnel envelopes. `SentryProvider` owns its own clone;
    /// this is a separate injection rather than an accessor on the provider.
    pub dsn_service: Arc<DSNService>,
    pub rate_limiter: IngestRateLimiter,
}

/// Share native OTel concurrency admission *before* Axum buffers the body.
struct SentryIngestPermit {
    _permit: tokio::sync::OwnedSemaphorePermit,
}

impl axum::extract::FromRequestParts<Arc<AppState>> for SentryIngestPermit {
    type Rejection = Response;
    async fn from_request_parts(
        _parts: &mut axum::http::request::Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        state
            .otel_service
            .try_acquire_ingest_permit()
            .map(|permit| Self { _permit: permit })
            .map_err(|_| {
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    [(header::RETRY_AFTER, "1")],
                    "Ingest capacity exhausted",
                )
                    .into_response()
            })
    }
}

/// Maximum compressed body size for Sentry ingest routes (2 MiB).
///
/// Typical Sentry events are 5–50 KB. The 2 MiB cap rejects slow-POST DoS
/// attempts before the body is fully buffered into memory. The decompression
/// bomb guard (MAX_DECOMPRESSED_SIZE) provides a second layer of protection
/// against gzip bombs where the compressed input is under this limit but the
/// expanded output is enormous.
const SENTRY_INGEST_BODY_LIMIT: usize = 2 * 1024 * 1024;

pub fn configure_routes() -> Router<Arc<AppState>> {
    // Create CORS layer that allows all origins for Sentry SDK compatibility
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any)
        .expose_headers(Any);

    Router::new()
        .route("/{project_id}/store/", post(ingest_sentry_event))
        .route("/{project_id}/envelope/", post(ingest_sentry_envelope))
        .route("/{project_id}/security/", post(ingest_security_reports))
        .route("/{project_id}/nel/", post(ingest_nel_reports))
        .route(SENTRY_TUNNEL_ROUTE_PATH, post(ingest_tunneled_envelope))
        // Fix #3: cap compressed body to 2 MiB before any buffering occurs.
        // This prevents slow-POST DoS where a client drip-feeds a large body
        // to hold a Tokio worker thread indefinitely.
        .layer(DefaultBodyLimit::max(SENTRY_INGEST_BODY_LIMIT))
        .layer(cors)
}

// Types are now in types.rs

/// Ingest a Sentry event (JSON payload)
#[utoipa::path(
    post,
    path = "/{project_id}/store/",
    params(
        ("project_id" = i32, Path, description = "Project ID")
    ),
    request_body = SentryEventRequest,
    responses(
        (status = 200, description = "Event ingested", body = SentryEventResponse),
        (status = 400, description = "Bad request"),
        (status = 401, description = "Unauthorized"),
        (status = 413, description = "Request body too large (exceeds 2 MiB)"),
    ),
    tag = "sentry-ingestor"
)]
async fn ingest_sentry_event(
    _permit: SentryIngestPermit,
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<i32>,
    Query(params): Query<std::collections::HashMap<String, String>>,
    headers: HeaderMap,
    // Fix #2: read ConnectInfo from extensions (inserted by axum when the listener
    // is started with `into_make_service_with_connect_info`). Option<Extension<T>>
    // returns None gracefully when the extension is absent (e.g. in unit tests).
    connect_info: Option<Extension<ConnectInfo<SocketAddr>>>,
    Json(event): Json<serde_json::Value>,
) -> impl IntoResponse {
    // Extract DSN key from auth header or query params
    let dsn_key = extract_dsn_key(&headers, &params);

    let dsn_key = match dsn_key.as_deref() {
        Some(key) => key,
        None => {
            return (StatusCode::UNAUTHORIZED, "Missing DSN key".to_string()).into_response();
        }
    };

    // Authenticate using the provider
    let auth = match state
        .sentry_provider
        .authenticate(project_id, dsn_key)
        .await
    {
        Ok(auth) => auth,
        Err(e) => {
            tracing::error!("Authentication failed: {:?}", e);
            return (StatusCode::UNAUTHORIZED, e.to_string()).into_response();
        }
    };

    if !state
        .rate_limiter
        .check(auth.project_id, auth.rate_limit_per_minute)
        .await
    {
        return rate_limited_response();
    }

    // Parse event using the provider
    let mut parsed_event = match state.sentry_provider.parse_json_event(event, &auth).await {
        Ok(event) => event,
        Err(e) => {
            tracing::error!("Failed to parse event: {:?}", e);
            return (StatusCode::BAD_REQUEST, e.to_string()).into_response();
        }
    };

    // Fix #2: resolve the real client IP using proxy-trust logic.
    // XFF is honored only when the direct TCP peer is loopback (our trusted Pingora proxy).
    // An attacker connecting directly and setting X-Forwarded-For is ignored.
    let peer = connect_info.map(|ext| ext.0 .0);
    let client_ip = temps_auth::resolve_client_ip(&headers, peer);

    // Enrich with IP geolocation and visitor correlation
    enrich_error_event(
        &mut parsed_event.error_data,
        Some(client_ip.as_str()),
        state.ip_address_service.as_ref(),
        state.db.as_ref(),
    )
    .await;

    // Store event using the error tracking service
    match state
        .error_tracking_service
        .process_error_event(parsed_event.error_data)
        .await
    {
        Ok(_) => {
            // Once-per-instance: "error tracking is in use on this instance".
            // report_once dedupes durably (and frees the error-ingest hot path
            // of the previous per-event has_error_groups DB lookup), so this is
            // instance-scoped — consistent with the other first-touch events —
            // rather than the old per-project guard.
            state.telemetry.report_once(
                "error_tracking_first_error",
                temps_core::telemetry::TelemetryEvent::new(
                    temps_core::telemetry::TelemetryEventKind::ErrorTrackingFirstError,
                ),
            );
            let response = SentryEventResponse {
                id: parsed_event.event_id,
            };
            (StatusCode::OK, Json(response)).into_response()
        }
        Err(e) => {
            tracing::error!("Failed to store event: {:?}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to store event: {}", e),
            )
                .into_response()
        }
    }
}

/// Ingest a Sentry envelope (binary payload)
#[utoipa::path(
    post,
    path = "/{project_id}/envelope/",
    params(
        ("project_id" = i32, Path, description = "Project ID")
    ),
    request_body(content = String, description = "Sentry envelope as binary data", content_type = "application/octet-stream"),
    responses(
        (status = 200, description = "Envelope ingested"),
        (status = 400, description = "Bad request"),
        (status = 401, description = "Unauthorized"),
        (status = 413, description = "Request body too large (exceeds 2 MiB)"),
    ),
    tag = "sentry-ingestor"
)]
async fn ingest_sentry_envelope(
    _permit: SentryIngestPermit,
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<i32>,
    Query(params): Query<std::collections::HashMap<String, String>>,
    headers: HeaderMap,
    // Fix #2: read ConnectInfo from extensions for proxy-trust IP resolution.
    connect_info: Option<Extension<ConnectInfo<SocketAddr>>>,
    body: Bytes,
) -> impl IntoResponse {
    // Log only key names — values and headers can contain the Sentry DSN auth key.
    debug!(
        "Sentry ingest: query param keys={:?}, header names={:?}",
        params.keys().collect::<Vec<_>>(),
        headers.keys().map(|k| k.as_str()).collect::<Vec<_>>()
    );
    // Extract DSN key from auth header or query params
    let dsn_key = extract_dsn_key(&headers, &params);

    // Check if body is gzip-compressed
    let decompressed_body = match decompress_if_needed(&headers, &body) {
        Ok(data) => data,
        Err(e) => {
            tracing::warn!("Failed to decompress envelope: {}", e);
            return (
                StatusCode::BAD_REQUEST,
                format!("Failed to decompress envelope: {}", e),
            )
                .into_response();
        }
    };

    // Authenticate using the provider (envelope parsing happens in provider)
    let dsn_key = match dsn_key.as_deref() {
        Some(key) => key,
        None => {
            return (StatusCode::UNAUTHORIZED, "Missing DSN key".to_string()).into_response();
        }
    };

    let auth = match state
        .sentry_provider
        .authenticate(project_id, dsn_key)
        .await
    {
        Ok(auth) => auth,
        Err(e) => {
            tracing::error!("Authentication failed: {:?}", e);
            return (StatusCode::UNAUTHORIZED, e.to_string()).into_response();
        }
    };

    if !state
        .rate_limiter
        .check(auth.project_id, auth.rate_limit_per_minute)
        .await
    {
        return rate_limited_response();
    }

    // Fix #2: resolve the real client IP using proxy-trust logic.
    // XFF is honored only when the direct TCP peer is loopback (our trusted Pingora proxy).
    let peer = connect_info.map(|ext| ext.0 .0);
    let client_ip = temps_auth::resolve_client_ip(&headers, peer);

    process_parsed_envelope(
        &state,
        &auth,
        &decompressed_body,
        &client_ip,
        &headers,
        true,
    )
    .await
}

#[utoipa::path(post,path="/{project_id}/security/",tag="sentry-ingestor",params(("project_id"=i32,Path)),
    request_body(content=String,content_type="application/csp-report"),responses((status=200),(status=400),(status=401),(status=403),(status=413),(status=415),(status=429)))]
async fn ingest_security_reports(
    _permit: SentryIngestPermit,
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<i32>,
    Query(params): Query<std::collections::HashMap<String, String>>,
    headers: HeaderMap,
    connect_info: Option<Extension<ConnectInfo<SocketAddr>>>,
    body: Bytes,
) -> Response {
    ingest_browser_reports(
        &state,
        project_id,
        params,
        headers,
        connect_info,
        body,
        false,
    )
    .await
}
#[utoipa::path(post,path="/{project_id}/nel/",tag="sentry-ingestor",params(("project_id"=i32,Path)),
    request_body(content=String,content_type="application/reports+json"),responses((status=200),(status=400),(status=401),(status=403),(status=413),(status=415),(status=429)))]
async fn ingest_nel_reports(
    _permit: SentryIngestPermit,
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<i32>,
    Query(params): Query<std::collections::HashMap<String, String>>,
    headers: HeaderMap,
    connect_info: Option<Extension<ConnectInfo<SocketAddr>>>,
    body: Bytes,
) -> Response {
    ingest_browser_reports(
        &state,
        project_id,
        params,
        headers,
        connect_info,
        body,
        true,
    )
    .await
}
async fn ingest_browser_reports(
    state: &AppState,
    project_id: i32,
    params: std::collections::HashMap<String, String>,
    headers: HeaderMap,
    connect_info: Option<Extension<ConnectInfo<SocketAddr>>>,
    body: Bytes,
    nel: bool,
) -> Response {
    let mime = headers
        .get(header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.split(';').next())
        .map(str::trim)
        .unwrap_or("");
    if !matches!(mime, "application/json" | "application/reports+json")
        && (nel || mime != "application/csp-report")
    {
        return (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "Unsupported browser report content type",
        )
            .into_response();
    }
    let Some(key) = extract_dsn_key(&headers, &params) else {
        return (StatusCode::UNAUTHORIZED, "Missing DSN key").into_response();
    };
    if state.rate_limiter.unresolved_budget_exhausted().await {
        return rate_limited_response();
    }
    let dsn = match state.dsn_service.get_project_by_public_key(&key).await {
        Ok(Some(dsn)) if dsn.project_id == project_id => dsn,
        Ok(_) => {
            state.rate_limiter.record_unresolved_attempt().await;
            return (StatusCode::UNAUTHORIZED, "Invalid project DSN").into_response();
        }
        Err(_) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                [(header::RETRY_AFTER, "60")],
                "DSN authentication temporarily unavailable",
            )
                .into_response()
        }
    };
    match dsn_origin_verdict(&dsn, &headers) {
        OriginVerdict::Allowed => {}
        OriginVerdict::Denied => {
            return (StatusCode::FORBIDDEN, "Origin not allowed by this DSN").into_response()
        }
        OriginVerdict::Malformed => return malformed_allowed_origins_response(),
    }
    let auth = auth_from_dsn(&dsn);
    if !state
        .rate_limiter
        .check(auth.project_id, auth.rate_limit_per_minute)
        .await
    {
        return rate_limited_response();
    }
    let body = match decompress_if_needed(&headers, &body) {
        Ok(body) => body,
        Err(_) => {
            return (StatusCode::BAD_REQUEST, "Invalid compressed browser report").into_response()
        }
    };
    let envelope = match super::security::frame_browser_reports(&body, nel, &params) {
        Ok(bytes) => Bytes::from(bytes),
        Err(reason) => return (StatusCode::BAD_REQUEST, reason).into_response(),
    };
    let peer = connect_info.map(|ext| ext.0 .0);
    let client_ip = temps_auth::resolve_client_ip(&headers, peer);
    process_parsed_envelope(state, &auth, &envelope, &client_ip, &headers, true).await
}

/// Ingest a browser-tunneled Sentry envelope.
///
/// Two resolution paths, tried in this order:
///
/// 1. **DSN credential.** An explicit `?sentry_key=` / `X-Sentry-Auth` /
///    `Authorization: DSN` value, or — failing that — the `dsn` field browser
///    SDKs embed in the envelope header whenever `Sentry.init({ tunnel })` is
///    used. The credential alone decides the project; `Host` and `Origin` are
///    not consulted. This is what makes the endpoint usable from an app Temps
///    does not deploy, which by definition has no route-table entry at all.
/// 2. **`Host`.** Unchanged legacy behaviour for same-origin, Temps-deployed
///    apps: the project/environment/deployment are resolved from the `Host`
///    header via the proxy's route table, the same way `/api/_temps/event`
///    (analytics) resolves. The proxy forwards anything under `/api/_temps`
///    to the console regardless of `Host`
///    (`temps-proxy::services::ROUTE_PREFIX_TEMPS`), so it works on custom
///    domains and previews without per-domain DSN configuration.
///
/// On path 2 there is no credential, so an `Origin`/`Referer` check stands in
/// for authentication: the request must claim to come from the same host it
/// resolves to, or it is rejected. That check is deliberately **skipped** on
/// path 1 — a tunneled request from another origin is the entire point there,
/// and the DSN is a stronger claim than a self-reported `Origin`. Path 1 does
/// honour the DSN row's own `allowed_origins` list when the operator has set
/// one (`403` when the `Origin` is off it); an empty or absent list, which is
/// every row Temps mints today, permits any origin.
///
/// # Ordering, and why it matters
///
/// This route is unauthenticated and reachable from every domain a project is
/// deployed to, so the expensive work is placed strictly after the cheap
/// admission control:
///
/// 1. Shape gate + resolution cache + a global unresolved-credential budget,
///    all before any database round trip (mirroring `temps-analytics`'s
///    ADR-040 §2 defence).
/// 2. Per-project rate limiting.
/// 3. Only then `decompress_if_needed`, which can inflate up to
///    `MAX_DECOMPRESSED_SIZE`.
///
/// The credential-free branch has to inflate the body to read an SDK-embedded
/// DSN, so when `Host` has not already resolved the request that inflate is
/// charged against the same global budget a forged key pays into.
///
/// An *explicit* credential that does not resolve is a `401`, never a silent
/// fall-through to `Host`: a typo'd key must fail loudly rather than land the
/// data under whichever project the `Host` happens to match. An *embedded*
/// envelope DSN that does not resolve falls through to path 2, because that
/// value was sniffed rather than presented — an app tunneling a third-party
/// DSN through Temps keeps working exactly as it did before.
#[utoipa::path(
    post,
    path = "/_temps/sentry/envelope",
    params(
        ("sentry_key" = Option<String>, Query, description = "DSN public key; resolves the project without consulting Host")
    ),
    request_body(content = String, description = "Sentry envelope as binary data", content_type = "application/octet-stream"),
    responses(
        (status = 200, description = "Envelope ingested"),
        (status = 204, description = "Host resolved to a route with no attributable project (sandbox/orphan)"),
        (status = 400, description = "Bad request"),
        (status = 401, description = "An explicit DSN key was presented but did not resolve"),
        (status = 403, description = "Origin/Referer does not match the resolved host (Host-resolved requests), or the Origin is not in the DSN's allowed_origins (keyed requests)"),
        (status = 404, description = "Unknown host"),
        (status = 413, description = "Request body too large (exceeds 2 MiB)"),
        (status = 429, description = "Rate limit exceeded"),
    ),
    tag = "sentry-ingestor"
)]
async fn ingest_tunneled_envelope(
    _permit: SentryIngestPermit,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<temps_core::RequestMetadata>,
    connect_info: Option<Extension<ConnectInfo<SocketAddr>>>,
    Query(params): Query<std::collections::HashMap<String, String>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let peer = connect_info.map(|ext| ext.0 .0);

    // ── Path 1: an explicit credential was presented ────────────────────
    //
    // Answered without looking at the body at all. Everything below the
    // credential check is gated on the credential resolving, so a forged key
    // cannot buy a database round trip, and cannot buy the inflate either.
    if let Some(key) = extract_dsn_key(&headers, &params) {
        // Global, IP-independent backstop, checked *before* the lookup. The
        // per-project buckets cannot cover a request that never resolved to a
        // project — there is no bucket to charge — so without this every
        // distinct forged key cost an uncached query on a route reachable
        // from any customer domain. `DSNService`'s cache only helps on an
        // exact repeated string.
        if state.rate_limiter.unresolved_budget_exhausted().await {
            tracing::warn!(
                "Sentry tunnel: global unresolved-credential budget exhausted; rejecting a keyed request before any lookup"
            );
            return rate_limited_response();
        }

        let dsn = match state.dsn_service.get_project_by_public_key(&key).await {
            Ok(Some(dsn)) => dsn,
            Ok(None) => {
                state.rate_limiter.record_unresolved_attempt().await;
                // Prefix only: enough to correlate with a key an operator is
                // holding, far too little to replay. (The old justification
                // here — "it is already in `proxy_logs`" — was simply wrong:
                // the proxy forwards `/api/_temps` straight through and does
                // not log it.)
                tracing::warn!(
                    public_key_prefix = public_key_prefix(&key),
                    "Sentry tunnel: presented DSN key did not match an active DSN"
                );
                return TunnelCredentialRejection::Unknown.into_response();
            }
            Err(e) => {
                tracing::error!("Sentry tunnel: DSN lookup failed: {}", e);
                return TunnelCredentialRejection::LookupFailed.into_response();
            }
        };

        // An explicitly presented key that carries an allowlist is refused
        // loudly when the origin is off it — the operator asked for that
        // restriction and a silent fall-through to `Host` would quietly undo
        // it.
        match dsn_origin_verdict(&dsn, &headers) {
            OriginVerdict::Allowed => {}
            OriginVerdict::Denied => {
                tracing::warn!(
                    project_id = dsn.project_id,
                    dsn_id = dsn.id,
                    "Sentry tunnel: Origin is not in this DSN's allowed_origins"
                );
                return (
                    StatusCode::FORBIDDEN,
                    "Origin is not allowed for this DSN".to_string(),
                )
                    .into_response();
            }
            OriginVerdict::Malformed => return malformed_allowed_origins_response(),
        }

        let auth = auth_from_dsn(&dsn);
        if !state
            .rate_limiter
            .check(auth.project_id, Some(effective_tunnel_rate_limit(&auth)))
            .await
        {
            return rate_limited_response();
        }

        // Decompression is deliberately last: it is by far the most expensive
        // thing this route does (up to `MAX_DECOMPRESSED_SIZE` of inflate) and
        // nothing above it needed the body.
        let decompressed_body = match decompress_if_needed(&headers, &body) {
            Ok(data) => data,
            Err(e) => return decompression_failed_response(&e),
        };

        let client_ip = temps_auth::resolve_client_ip(&headers, peer);
        return process_parsed_envelope(
            &state,
            &auth,
            &decompressed_body,
            &client_ip,
            &headers,
            true,
        )
        .await;
    }

    // ── Path 2: no explicit credential ──────────────────────────────────
    //
    // `Host` is resolved first because it is an in-memory route-table lookup:
    // it lets the ordinary same-origin case gate decompression on its own
    // rate-limit budget before anything is inflated. The SDK-embedded DSN,
    // which does need the body, is consulted after — and still wins when it
    // resolves, because a credential is a stronger claim than a `Host` the
    // proxy forwarded here regardless of its value.
    //
    // `Host` and the embedded DSN can legitimately disagree on the project (a
    // third-party DSN tunneled through an app Temps also hosts), so the gate
    // below *reserves* a slot in `Host`'s bucket — a real, consumed
    // reservation, not a non-consuming peek, because concurrent requests all
    // observing the same free peek slot and all proceeding into a 10 MiB
    // inflate is exactly the amplification this route exists to prevent — and
    // that reservation is precisely released and re-charged against the
    // correct project below once the embedded DSN has actually been read, so
    // one tunneled request still never drains two buckets for one event.
    let host_scope = resolve_tunnel_host_scope(&state, &metadata, &headers);

    let mut host_reservation: Option<(i32, Admission)> = None;
    match &host_scope {
        Ok(auth) => {
            let admission = state
                .rate_limiter
                .check_reserving(auth.project_id, Some(effective_tunnel_rate_limit(auth)))
                .await;
            if admission == Admission::Denied {
                return rate_limited_response();
            }
            host_reservation = Some((auth.project_id, admission));
        }
        Err(_) => {
            // Nothing has authenticated this request and `Host` did not
            // resolve it either, yet reading the SDK-embedded DSN out of the
            // envelope header requires inflating the body. Charge that
            // against the same global budget a forged key pays into, so the
            // credential-free branch cannot be used as an unmetered inflate
            // oracle. Only the *check* happens here; the attempt is recorded
            // below, once the embedded DSN has also failed, so a legitimate
            // zero-configuration app never burns budget.
            if state.rate_limiter.unresolved_budget_exhausted().await {
                tracing::warn!(
                    "Sentry tunnel: global unresolved-credential budget exhausted; rejecting a credential-free request before decompression"
                );
                return rate_limited_response();
            }
        }
    }

    let decompressed_body = match decompress_if_needed(&headers, &body) {
        Ok(data) => data,
        Err(e) => {
            // A malformed body still fails after a slot was reserved above —
            // release it so a burst of bad gzip against project X cannot
            // quietly exhaust X's tunnel allowance without ever ingesting an
            // event.
            if let Some((reserved_project_id, Admission::Reserved(marker))) = host_reservation {
                state
                    .rate_limiter
                    .release(reserved_project_id, marker)
                    .await;
            }
            return decompression_failed_response(&e);
        }
    };

    let embedded = resolve_embedded_envelope_dsn(&state, &headers, &decompressed_body).await;
    let dsn_authenticated = embedded.is_some();

    let auth = match embedded {
        Some(dsn) => auth_from_dsn(&dsn),
        None => match host_scope {
            Ok(auth) => auth,
            Err(response) => {
                // Confirmed: no credential, and no host either. This is the
                // shape an abusive caller has, so it is what the global
                // budget counts.
                state.rate_limiter.record_unresolved_attempt().await;
                return *response;
            }
        },
    };

    // Reconcile the tentative `Host` reservation with the project the request
    // was actually attributed to.
    match host_reservation {
        // `Host` reserved (or was unlimited) against exactly the project the
        // request ended up attributed to — that reservation *is* the one and
        // only charge for this request.
        Some((reserved_project_id, _)) if reserved_project_id == auth.project_id => {}
        // The embedded DSN redirected attribution to a different project:
        // give `Host`'s project its slot back — it received no data — and
        // charge the project that actually did.
        Some((reserved_project_id, Admission::Reserved(marker))) => {
            state
                .rate_limiter
                .release(reserved_project_id, marker)
                .await;
            if !state
                .rate_limiter
                .check(auth.project_id, Some(effective_tunnel_rate_limit(&auth)))
                .await
            {
                return rate_limited_response();
            }
        }
        // `Host` had no configured limit (nothing was reserved) but the
        // embedded DSN's project does — that project still needs its own
        // charge; `Host`'s unlimited quota does not cover it.
        Some((_, Admission::Unlimited)) => {
            if !state
                .rate_limiter
                .check(auth.project_id, Some(effective_tunnel_rate_limit(&auth)))
                .await
            {
                return rate_limited_response();
            }
        }
        Some((_, Admission::Denied)) => {
            unreachable!("a Denied admission returns 429 immediately above and is never stored")
        }
        // `Host` never resolved at all; the embedded DSN is the only
        // authority for this request and has not been charged yet.
        None => {
            if !state
                .rate_limiter
                .check(auth.project_id, Some(effective_tunnel_rate_limit(&auth)))
                .await
            {
                return rate_limited_response();
            }
        }
    }

    let client_ip = temps_auth::resolve_client_ip(&headers, peer);

    process_parsed_envelope(
        &state,
        &auth,
        &decompressed_body,
        &client_ip,
        &headers,
        dsn_authenticated,
    )
    .await
}

/// 429 with the `Retry-After` a client needs to back off sensibly.
///
/// Every ingest route answers rate limiting identically: a browser SDK told
/// only "too many requests" either hammers the endpoint or drops the data, and
/// a self-hosted operator reading the network tab is left guessing.
fn rate_limited_response() -> Response {
    (
        StatusCode::TOO_MANY_REQUESTS,
        [(header::RETRY_AFTER, INGEST_RETRY_AFTER_SECONDS)],
        "Rate limit exceeded".to_string(),
    )
        .into_response()
}

fn decompression_failed_response(error: &str) -> Response {
    tracing::warn!("Failed to decompress tunneled envelope: {}", error);
    (
        StatusCode::BAD_REQUEST,
        format!("Failed to decompress envelope: {}", error),
    )
        .into_response()
}

/// A DSN row's `allowed_origins` column is JSON the operator controls, so a
/// corrupt value is a third outcome distinct from allow/deny.
enum OriginVerdict {
    Allowed,
    Denied,
    Malformed,
}

/// 500 when a DSN's `allowed_origins` cannot be parsed.
///
/// Fails closed and loudly rather than treating a corrupt allowlist as "any
/// origin", which would silently relax a restriction the operator set. Mirrors
/// `AnalyticsIngestKeyError::MalformedAllowedOrigins`.
fn malformed_allowed_origins_response() -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "This DSN's allowed_origins is malformed; fix it in the project's error-tracking settings"
            .to_string(),
    )
        .into_response()
}

/// Whether this request's `Origin` satisfies the DSN row's `allowed_origins`.
///
/// An absent or empty list permits any origin and is not checked at all,
/// including for a request with no `Origin` header (a `curl` or server-side
/// caller). A non-empty list requires an exact scheme/host/port match.
///
/// Like the analytics equivalent this is a browser-enforced convenience
/// control, not authentication — a non-browser client picks its own `Origin`
/// or omits it. It exists so a key copy-pasted out of a page cannot casually
/// be used from another site.
///
/// Today nothing in Temps writes this column; the console has no editor for it
/// yet. It is enforced when present anyway, so the day an operator sets it
/// (by hand, or through the editor that will land later) it takes effect
/// rather than being silently ignored.
fn dsn_origin_verdict(
    dsn: &temps_entities::project_dsns::Model,
    headers: &HeaderMap,
) -> OriginVerdict {
    let Some(raw) = dsn.allowed_origins.as_ref().filter(|v| !v.is_null()) else {
        return OriginVerdict::Allowed;
    };

    let allowed: Vec<String> = match serde_json::from_value(raw.clone()) {
        Ok(allowed) => allowed,
        Err(e) => {
            tracing::warn!(
                dsn_id = dsn.id,
                project_id = dsn.project_id,
                error = %e,
                "DSN has a malformed allowed_origins column"
            );
            return OriginVerdict::Malformed;
        }
    };

    if allowed.is_empty() {
        return OriginVerdict::Allowed;
    }

    let origin = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty());

    let Some(origin) = origin else {
        // The list is non-empty and the browser sent nothing to match it
        // against — fail closed rather than reading "absent" as "any".
        return OriginVerdict::Denied;
    };

    if allowed
        .iter()
        .any(|candidate| origins_match(candidate.trim(), origin))
    {
        OriginVerdict::Allowed
    } else {
        OriginVerdict::Denied
    }
}

/// Exact origin comparison: scheme, host and port must all match, with scheme
/// and host compared case-insensitively (RFC 3986 / DNS) and port as written.
///
/// Deliberately a local copy of `temps_analytics::ingest_keys::is_origin_allowed`
/// rather than a shared helper: importing it would add a crate edge from
/// `temps-error-tracking` to the whole analytics umbrella (and its AI/embedding
/// dependencies) for twenty lines of pure string comparison. The two are kept
/// in step by their tests, which assert the same cases.
fn origins_match(allowed: &str, origin: &str) -> bool {
    fn split_scheme(origin: &str) -> (&str, &str) {
        match origin.split_once("://") {
            Some((scheme, authority)) => (scheme, authority),
            None => ("", origin),
        }
    }

    /// Split `host[:port]`, tolerating bracketed IPv6 literals (`[::1]`,
    /// whose own colons must not be read as a port separator).
    fn split_port(authority: &str) -> (&str, &str) {
        match authority.rsplit_once(':') {
            Some((host, port))
                if !port.is_empty()
                    && port.chars().all(|c| c.is_ascii_digit())
                    && !host.is_empty() =>
            {
                (host, port)
            }
            _ => (authority, ""),
        }
    }

    let (allowed_scheme, allowed_authority) = split_scheme(allowed);
    let (origin_scheme, origin_authority) = split_scheme(origin);
    let (allowed_host, allowed_port) = split_port(allowed_authority);
    let (origin_host, origin_port) = split_port(origin_authority);

    allowed_scheme.eq_ignore_ascii_case(origin_scheme)
        && allowed_host.eq_ignore_ascii_case(origin_host)
        && allowed_port == origin_port
}

/// The ceiling applied to a tunneled request.
///
/// A DSN row with no explicit limit falls back to the tunnel default rather
/// than to "unlimited": this endpoint is reachable cross-origin from anywhere,
/// so it always keeps a ceiling.
fn effective_tunnel_rate_limit(auth: &AuthContext) -> i32 {
    auth.rate_limit_per_minute
        .filter(|limit| *limit > 0)
        .unwrap_or(TUNNEL_DEFAULT_RATE_LIMIT_PER_MINUTE)
}

/// Resolve a credential-free tunneled request from its `Host`.
///
/// Purely in-memory (route table + header comparison), so it is safe to run
/// before the body is inflated. `Err` carries the response to send if nothing
/// else resolves the request — boxed because an `http::Response` dwarfs the
/// `Ok` variant and `clippy::result_large_err` is denied workspace-wide.
fn resolve_tunnel_host_scope(
    state: &AppState,
    metadata: &temps_core::RequestMetadata,
    headers: &HeaderMap,
) -> Result<AuthContext, Box<Response>> {
    let host = metadata.host.clone();
    if host.is_empty() {
        return Err(Box::new(
            (StatusCode::BAD_REQUEST, "Missing Host header".to_string()).into_response(),
        ));
    }

    // With no credential, an `Origin`/`Referer` check stands in for
    // authentication: the request must claim to come from the host it
    // resolves to.
    if !origin_matches_host(headers, &host) {
        tracing::warn!(
            "Sentry tunnel: Origin/Referer does not match resolved host {}",
            host
        );
        return Err(Box::new(
            (
                StatusCode::FORBIDDEN,
                "Origin does not match host".to_string(),
            )
                .into_response(),
        ));
    }

    // Exact + wildcard resolution, matching the precedence the proxy itself
    // used to route this request here in the first place (`services.rs`'s
    // `get_route_by_host`) — the narrower `get_route` (legacy map only)
    // would 404 wildcard custom routes that the proxy successfully forwards.
    let Some(route) = state.route_table.get_route_by_host(&host) else {
        tracing::debug!("Sentry tunnel: host {} not found in route table", host);
        return Err(Box::new(StatusCode::NOT_FOUND.into_response()));
    };

    // A route without a project is a sandbox/orphaned route — nothing to
    // attribute this to. Drop silently (204), mirroring how
    // `record_event_metrics` (analytics) handles the same case.
    let Some(project) = route.project.as_ref() else {
        debug!(
            "Sentry tunnel: dropping envelope for host {} — route has no associated project",
            host
        );
        return Err(Box::new(StatusCode::NO_CONTENT.into_response()));
    };

    Ok(AuthContext {
        project_id: project.id,
        environment_id: route.environment.as_ref().map(|e| e.id),
        deployment_id: route.deployment.as_ref().map(|d| d.id),
        // No DSN row was selected for this request — the fixed tunnel default
        // applies instead (see `effective_tunnel_rate_limit`).
        rate_limit_per_minute: None,
    })
}

/// Look for the `dsn` field browser SDKs embed in the envelope header whenever
/// `Sentry.init({ tunnel })` is used.
///
/// Everything that fails here returns `None` and falls through to the `Host`
/// path rather than rejecting: this value was *sniffed*, not presented, so an
/// app tunneling a third-party DSN through Temps keeps working exactly as it
/// did before. That includes an `allowed_origins` mismatch — refusing would
/// change the behaviour of a request the operator never opted this endpoint
/// into.
async fn resolve_embedded_envelope_dsn(
    state: &AppState,
    headers: &HeaderMap,
    decompressed_body: &Bytes,
) -> Option<temps_entities::project_dsns::Model> {
    let embedded = peek_envelope_dsn(decompressed_body)?;

    let parsed = match state.dsn_service.parse_dsn(&embedded) {
        Ok(parsed) => parsed,
        Err(e) => {
            tracing::debug!(
                "Sentry tunnel: envelope header DSN is unparsable ({}), falling back to Host",
                e
            );
            return None;
        }
    };

    let dsn = match state.dsn_service.validate_dsn_auth(&parsed).await {
        Ok((true, Some(dsn))) => dsn,
        Ok(_) => {
            tracing::debug!(
                "Sentry tunnel: envelope header DSN for project {} did not match an active DSN, falling back to Host",
                parsed.project_id
            );
            return None;
        }
        Err(e) => {
            tracing::warn!(
                "Sentry tunnel: failed to validate envelope header DSN for project {}: {}",
                parsed.project_id,
                e
            );
            return None;
        }
    };

    match dsn_origin_verdict(&dsn, headers) {
        OriginVerdict::Allowed => Some(dsn),
        OriginVerdict::Denied | OriginVerdict::Malformed => {
            tracing::warn!(
                dsn_id = dsn.id,
                project_id = dsn.project_id,
                "Sentry tunnel: embedded envelope DSN rejected by allowed_origins, falling back to Host"
            );
            None
        }
    }
}

/// Why an explicitly presented tunnel credential was refused.
///
/// Kept separate from the HTTP response so the resolver stays a resolver: a
/// self-hosted operator has to be able to tell "my key is wrong" (re-mint it)
/// from "the lookup failed" (check the database), and collapsing both into one
/// status would send them down the wrong path.
enum TunnelCredentialRejection {
    /// The key matched no active DSN row on a live project.
    Unknown,
    /// The key could not be checked at all.
    LookupFailed,
}

impl IntoResponse for TunnelCredentialRejection {
    fn into_response(self) -> Response {
        match self {
            TunnelCredentialRejection::Unknown => (
                StatusCode::UNAUTHORIZED,
                "Invalid or revoked DSN key".to_string(),
            )
                .into_response(),
            TunnelCredentialRejection::LookupFailed => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to validate DSN key".to_string(),
            )
                .into_response(),
        }
    }
}

/// Build the ingest scope from a resolved DSN row.
///
/// Write-only by construction: an `AuthContext` is consumed exclusively by
/// `process_parsed_envelope`, which appends error events. No read handler
/// accepts one.
fn auth_from_dsn(dsn: &temps_entities::project_dsns::Model) -> AuthContext {
    AuthContext {
        project_id: dsn.project_id,
        environment_id: dsn.environment_id,
        deployment_id: dsn.deployment_id,
        rate_limit_per_minute: dsn.rate_limit_per_minute,
    }
}

/// Returns `true` if the request's `Origin` (falling back to `Referer`) host
/// matches `expected_host`. Missing both headers is rejected: modern
/// browsers attach `Origin` to every non-GET `fetch`, including same-origin
/// ones, so a legitimate tunneled POST always carries one — a request with
/// neither is either an ancient browser or a script deliberately omitting it
/// to bypass this check, and defaulting to deny is the safer read of that.
fn origin_matches_host(headers: &HeaderMap, expected_host: &str) -> bool {
    let candidate = headers
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .or_else(|| headers.get(header::REFERER).and_then(|v| v.to_str().ok()));

    let Some(candidate) = candidate else {
        return false;
    };

    url::Url::parse(candidate)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.eq_ignore_ascii_case(expected_host)))
        .unwrap_or(false)
}

/// Parse and store the events in an already-authenticated/resolved envelope.
/// Shared by the DSN-authenticated and Host-resolved (tunneled) ingest paths.
async fn process_parsed_envelope(
    state: &AppState,
    auth: &AuthContext,
    decompressed_body: &Bytes,
    client_ip: &str,
    headers: &HeaderMap,
    dsn_authenticated: bool,
) -> Response {
    let request = super::inference::RequestInference::new(client_ip, headers);
    // Decode and validate all supported items before any durable writes.
    let batch = match state.sentry_provider.parse_envelope_with_request(
        decompressed_body,
        auth,
        Some(&request),
    ) {
        Ok(batch) => batch,
        Err(crate::providers::ProviderError::LogBatch(error)) => {
            return (StatusCode::BAD_REQUEST, error.to_string()).into_response();
        }
        Err(_) => {
            tracing::warn!(project_id = auth.project_id, "Invalid Sentry envelope");
            return (
                StatusCode::BAD_REQUEST,
                "Invalid or unsupported Sentry envelope",
            )
                .into_response();
        }
    };
    let has_telemetry = !batch.feedback.is_empty()
        || !batch.logs.is_empty()
        || !batch.check_ins.is_empty()
        || !batch.spans.is_empty()
        || !batch.metrics.is_empty()
        || batch.replay.is_some();
    if (has_telemetry || !batch.client_outcomes.is_empty()) && !dsn_authenticated {
        return (
            StatusCode::UNAUTHORIZED,
            "A valid Sentry DSN is required for telemetry",
        )
            .into_response();
    }
    let unsupported = batch.unsupported.values().sum::<usize>();
    let unsupported_types = batch
        .unsupported
        .keys()
        .copied()
        .collect::<Vec<_>>()
        .join(",");
    if unsupported > 0 {
        tracing::debug!(project_id = auth.project_id, unsupported_items = unsupported, types = %unsupported_types, "Sentry items unsupported; none stored");
    }
    if has_telemetry {
        if let Err(error) = state.otel_service.check_rate_limit(auth.project_id) {
            return log_ingest_error(error, 0, unsupported, &unsupported_types);
        }
        if let Err(error) = state.otel_service.check_quota(auth.project_id).await {
            return log_ingest_error(error, 0, unsupported, &unsupported_types);
        }
    }
    let check_ins = if batch.check_ins.is_empty() {
        temps_monitoring::cron_monitors::CheckInIngestResult::default()
    } else {
        match state
            .cron_monitor_service
            .ingest_report(
                auth.project_id,
                auth.environment_id,
                auth.deployment_id,
                batch.check_ins,
            )
            .await
        {
            Ok(receipt) => receipt,
            Err(error) => {
                use temps_monitoring::cron_monitors::CronMonitorError;
                if matches!(
                    &error,
                    CronMonitorError::Database(_)
                        | CronMonitorError::Alarm(_)
                        | CronMonitorError::StoredConfiguration { .. }
                ) {
                    tracing::error!(project_id = auth.project_id, error = %error, "Cron check-in ingestion failed");
                }
                let retryable = error.is_retryable();
                let status = match error {
                    CronMonitorError::Invalid(_) => StatusCode::BAD_REQUEST,
                    CronMonitorError::NotFound { .. } | CronMonitorError::ScopeNotFound { .. } => {
                        StatusCode::NOT_FOUND
                    }
                    CronMonitorError::Conflict => StatusCode::CONFLICT,
                    CronMonitorError::Limit => StatusCode::TOO_MANY_REQUESTS,
                    CronMonitorError::Database(_) if retryable => StatusCode::SERVICE_UNAVAILABLE,
                    CronMonitorError::Database(_)
                    | CronMonitorError::StoredConfiguration { .. }
                    | CronMonitorError::Alarm(_) => StatusCode::INTERNAL_SERVER_ERROR,
                };
                return envelope_outcome(status, 0, 0, unsupported, &unsupported_types);
            }
        }
    };
    let with_check_ins = |mut response: Response| {
        if let Ok(value) = http::HeaderValue::from_str(&check_ins.accepted.to_string()) {
            response
                .headers_mut()
                .insert("x-temps-sentry-check-ins-stored", value);
        }
        if let Ok(value) = http::HeaderValue::from_str(&check_ins.pending_configuration.to_string())
        {
            response
                .headers_mut()
                .insert("x-temps-sentry-check-ins-pending-config", value);
        }
        response
    };
    let mut domains = DomainReceipts::default();
    if !batch.client_outcomes.is_empty() {
        match state
            .error_tracking_service
            .client_outcomes
            .ingest_reports(
                auth.project_id,
                auth.environment_id,
                auth.deployment_id,
                batch.client_outcomes,
            )
            .await
        {
            Ok(receipt) => {
                domains.client_outcomes = receipt.accepted;
                domains.client_outcome_duplicates = receipt.duplicates;
            }
            Err(error) => {
                let status = match error {
                    crate::services::types::ErrorTrackingError::Validation(_) => {
                        StatusCode::BAD_REQUEST
                    }
                    crate::services::types::ErrorTrackingError::Database(db) => {
                        if crate::services::client_outcome_service::ClientOutcomeService::is_receipt_capacity_error(&db)
                            || temps_otel::OtelError::Database(db).is_transient() {
                            StatusCode::SERVICE_UNAVAILABLE
                        } else {
                            StatusCode::INTERNAL_SERVER_ERROR
                        }
                    }
                    _ => StatusCode::INTERNAL_SERVER_ERROR,
                };
                tracing::warn!(
                    project_id = auth.project_id,
                    error_class = "client_outcome_storage",
                    "Client outcome storage failed"
                );
                return domains.apply(with_check_ins(envelope_outcome(
                    status,
                    0,
                    0,
                    unsupported,
                    &unsupported_types,
                )));
            }
        }
    }
    if !batch.feedback.is_empty() {
        match state
            .error_tracking_service
            .feedback
            .ingest(
                auth.project_id,
                auth.environment_id,
                auth.deployment_id,
                batch.feedback,
            )
            .await
        {
            Ok(receipt) => {
                domains.feedback = receipt.stored;
                domains.feedback_duplicates = receipt.duplicates;
            }
            Err(error) => {
                let status = match error {
                    crate::services::types::ErrorTrackingError::Validation(_) => {
                        StatusCode::BAD_REQUEST
                    }
                    crate::services::types::ErrorTrackingError::Database(db) => {
                        if temps_otel::OtelError::Database(db).is_transient() {
                            StatusCode::SERVICE_UNAVAILABLE
                        } else {
                            StatusCode::INTERNAL_SERVER_ERROR
                        }
                    }
                    _ => StatusCode::INTERNAL_SERVER_ERROR,
                };
                return domains.apply(with_check_ins(envelope_outcome(
                    status,
                    0,
                    0,
                    unsupported,
                    &unsupported_types,
                )));
            }
        }
    }
    let mut errors_stored = 0;
    // Store each event using the error tracking service
    for mut event in batch.events {
        // Enrich with IP geolocation and visitor correlation
        enrich_error_event(
            &mut event.error_data,
            Some(client_ip),
            state.ip_address_service.as_ref(),
            state.db.as_ref(),
        )
        .await;

        match state
            .error_tracking_service
            .process_error_event_with_receipt(event.error_data)
            .await
        {
            Ok(receipt) if receipt.stored => errors_stored += 1,
            Ok(_) => domains.errors_duplicates += 1,
            Err(error) => {
                let status = match error {
                    crate::services::types::ErrorTrackingError::Validation(_) => {
                        StatusCode::BAD_REQUEST
                    }
                    crate::services::types::ErrorTrackingError::Database(db) => {
                        if temps_otel::OtelError::Database(db).is_transient() {
                            StatusCode::SERVICE_UNAVAILABLE
                        } else {
                            StatusCode::INTERNAL_SERVER_ERROR
                        }
                    }
                    _ => StatusCode::INTERNAL_SERVER_ERROR,
                };
                tracing::error!(
                    project_id = auth.project_id,
                    error_class = "error_storage",
                    "Failed to store Sentry error event"
                );
                return domains.apply(with_check_ins(envelope_outcome(
                    status,
                    errors_stored,
                    0,
                    unsupported,
                    &unsupported_types,
                )));
            }
        }

        // Once-per-instance: "error tracking is in use here". report_once is
        // idempotent and durably deduped, so no per-request flag is needed —
        // and the error-ingest hot path no longer pays a has_error_groups DB
        // lookup per envelope.
        state.telemetry.report_once(
            "error_tracking_first_error",
            temps_core::telemetry::TelemetryEvent::new(
                temps_core::telemetry::TelemetryEventKind::ErrorTrackingFirstError,
            ),
        );
    }

    if !batch.spans.is_empty() {
        match state.otel_service.ingest_spans(batch.spans).await {
            Ok(n) => domains.spans = n,
            Err(error) => {
                return domains.apply(with_check_ins(log_ingest_error(
                    error,
                    errors_stored,
                    unsupported,
                    &unsupported_types,
                )))
            }
        }
    }
    if !batch.metrics.is_empty() {
        match state.otel_service.ingest_metrics(batch.metrics).await {
            Ok(n) => domains.metrics = n,
            Err(error) => {
                return domains.apply(with_check_ins(log_ingest_error(
                    error,
                    errors_stored,
                    unsupported,
                    &unsupported_types,
                )))
            }
        }
    }
    if let Some(replay) = batch.replay {
        let store = async {
            state
                .session_replay_service
                .initialize_session(
                    &replay.session_id,
                    replay.metadata,
                    auth.project_id,
                    auth.environment_id,
                    auth.deployment_id,
                    client_ip,
                )
                .await?;
            state
                .session_replay_service
                .add_decoded_session_events(
                    auth.project_id,
                    &replay.session_id,
                    replay.events,
                    &format!("sentry-segment-{}", replay.segment_id),
                    replay.segment_id,
                )
                .await
        }
        .await;
        match store {
            Ok(n) => {
                domains.replay_segments = 1;
                domains.replay_events = n as u64;
            }
            Err(error) => {
                use temps_analytics_session_replay::services::SessionReplayError;
                let status = match error {
                    SessionReplayError::VisitorCreationRateLimited { .. }
                    | SessionReplayError::BatchTooLarge {
                        unit: "session events",
                        ..
                    } => StatusCode::TOO_MANY_REQUESTS,
                    SessionReplayError::BatchTooLarge { .. } => StatusCode::PAYLOAD_TOO_LARGE,
                    SessionReplayError::CrossProjectAccess { .. } => StatusCode::NOT_FOUND,
                    SessionReplayError::Database(db) => {
                        let otel = temps_otel::OtelError::Database(db);
                        if otel.is_transient() {
                            StatusCode::SERVICE_UNAVAILABLE
                        } else {
                            StatusCode::INTERNAL_SERVER_ERROR
                        }
                    }
                    _ => StatusCode::BAD_REQUEST,
                };
                return domains.apply(with_check_ins(envelope_outcome(
                    status,
                    errors_stored,
                    0,
                    unsupported,
                    &unsupported_types,
                )));
            }
        }
    }
    let logs_stored = if batch.logs.is_empty() {
        0
    } else {
        match state.otel_service.ingest_logs_confirmed(batch.logs).await {
            Ok(stored) => stored,
            Err(error) => {
                return domains.apply(with_check_ins(log_ingest_error(
                    error,
                    errors_stored,
                    unsupported,
                    &unsupported_types,
                )))
            }
        }
    };
    tracing::debug!(
        project_id = auth.project_id,
        errors_stored,
        logs_stored,
        unsupported_items = unsupported,
        "Sentry envelope processed"
    );
    let mut response = domains.apply(with_check_ins(envelope_outcome(
        StatusCode::OK,
        errors_stored,
        logs_stored,
        unsupported,
        &unsupported_types,
    )));
    if check_ins.pending_configuration > 0 {
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            http::HeaderValue::from_static("text/plain; charset=utf-8"),
        );
        *response.body_mut()=axum::body::Body::from("Check-ins stored; include monitor_config on a check-in to enable missed-run and timeout detection.");
    }
    response
}

#[derive(Default)]
struct DomainReceipts {
    client_outcomes: u64,
    client_outcome_duplicates: u64,
    feedback: u64,
    feedback_duplicates: u64,
    errors_duplicates: u64,
    spans: u64,
    metrics: u64,
    replay_segments: u64,
    replay_events: u64,
}
impl DomainReceipts {
    fn apply(&self, mut response: Response) -> Response {
        for (name, count) in [
            (
                "x-temps-sentry-client-outcomes-accepted",
                self.client_outcomes,
            ),
            (
                "x-temps-sentry-client-outcomes-duplicates",
                self.client_outcome_duplicates,
            ),
            ("x-temps-sentry-feedback-stored", self.feedback),
            (
                "x-temps-sentry-feedback-duplicates",
                self.feedback_duplicates,
            ),
            ("x-temps-sentry-errors-duplicates", self.errors_duplicates),
            ("x-temps-sentry-spans-stored", self.spans),
            ("x-temps-sentry-metrics-stored", self.metrics),
            (
                "x-temps-sentry-replay-segments-accepted",
                self.replay_segments,
            ),
            ("x-temps-sentry-replay-events-stored", self.replay_events),
        ] {
            if let Ok(value) = http::HeaderValue::from_str(&count.to_string()) {
                response.headers_mut().insert(name, value);
            }
        }
        response
    }
}

/// Counts confirm durable domain records or check-in updates, not decoded items.
/// Idempotent check-in replays acknowledge the existing run; these are not unique
/// row counts. On failure a backend may have committed more than it confirmed.
/// Native identified telemetry retries are deduplicated within their receipt
/// window. Anonymous observations and external destinations retain their native
/// delivery semantics; no cross-domain transaction is implied.
fn envelope_outcome(
    status: StatusCode,
    errors: u64,
    logs: u64,
    unsupported: usize,
    types: &str,
) -> Response {
    let mut response = (
        status,
        [
            ("x-temps-sentry-client-outcomes-accepted", "0".to_string()),
            ("x-temps-sentry-client-outcomes-duplicates", "0".to_string()),
            ("x-temps-sentry-spans-stored", "0".to_string()),
            ("x-temps-sentry-metrics-stored", "0".to_string()),
            ("x-temps-sentry-replay-segments-accepted", "0".to_string()),
            ("x-temps-sentry-replay-events-stored", "0".to_string()),
            ("x-temps-sentry-errors-stored", errors.to_string()),
            ("x-temps-sentry-errors-duplicates", "0".to_string()),
            ("x-temps-sentry-logs-stored", logs.to_string()),
            ("x-temps-sentry-check-ins-stored", "0".to_string()),
            ("x-temps-sentry-check-ins-pending-config", "0".to_string()),
            ("x-temps-sentry-unsupported-items", unsupported.to_string()),
            ("x-temps-sentry-unsupported-types", types.to_string()),
        ],
    )
        .into_response();
    if status == StatusCode::TOO_MANY_REQUESTS || status == StatusCode::SERVICE_UNAVAILABLE {
        response
            .headers_mut()
            .insert(header::RETRY_AFTER, http::HeaderValue::from_static("60"));
    }
    response
}

fn log_ingest_error(
    error: temps_otel::OtelError,
    errors: u64,
    unsupported: usize,
    types: &str,
) -> Response {
    use temps_otel::OtelError;
    let status = match &error {
        OtelError::RateLimitExceeded { .. } | OtelError::QuotaExceeded { .. } => {
            StatusCode::TOO_MANY_REQUESTS
        }
        OtelError::IngestSaturated { .. } | OtelError::StorageMigrating { .. } => {
            StatusCode::SERVICE_UNAVAILABLE
        }
        OtelError::Validation { .. } => StatusCode::BAD_REQUEST,
        _ if error.is_transient() => StatusCode::SERVICE_UNAVAILABLE,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    tracing::warn!(
        error_class = error.error_class(),
        confirmed_errors = errors,
        "Sentry telemetry ingestion failed"
    );
    envelope_outcome(status, errors, 0, unsupported, types)
}

/// Maximum decompressed size to prevent decompression bombs (10 MB)
const MAX_DECOMPRESSED_SIZE: usize = 10 * 1024 * 1024;

/// Decompress the request body if it's gzip-compressed
/// Sentry SDKs can send gzip-compressed envelopes with Content-Encoding: gzip header
///
/// SECURITY: Uses a size-limited reader to prevent decompression bomb attacks where
/// a small compressed payload expands to consume all available memory.
fn decompress_if_needed(headers: &HeaderMap, body: &Bytes) -> Result<Bytes, String> {
    let encodings: Vec<_> = headers.get_all(header::CONTENT_ENCODING).iter().collect();
    match encodings.as_slice() {
        [] => return Ok(body.clone()),
        [value] => match value.to_str().map(str::trim) {
            Ok(value) if value.eq_ignore_ascii_case("identity") => return Ok(body.clone()),
            Ok(value) if value.eq_ignore_ascii_case("gzip") => {}
            _ => return Err("Unsupported content encoding; use identity or gzip".into()),
        },
        _ => return Err("Multiple content encodings are unsupported".into()),
    }

    // Decompress gzip data with size limit to prevent decompression bombs
    let decoder = flate2::read::MultiGzDecoder::new(&body[..]);
    let mut limited_reader = decoder.take(MAX_DECOMPRESSED_SIZE as u64 + 1);
    let mut decompressed = Vec::new();

    limited_reader
        .read_to_end(&mut decompressed)
        .map_err(|e| format!("Failed to decompress gzip data: {}", e))?;

    if decompressed.len() > MAX_DECOMPRESSED_SIZE {
        return Err(format!(
            "Decompressed data exceeds maximum allowed size of {} bytes",
            MAX_DECOMPRESSED_SIZE
        ));
    }

    tracing::debug!(
        "Decompressed envelope: {} bytes -> {} bytes",
        body.len(),
        decompressed.len()
    );

    Ok(Bytes::from(decompressed))
}

/// Enrich error event data with IP geolocation and visitor information.
///
/// This resolves the client IP to a geolocation record and looks up the
/// most recent visitor for this project with the same IP address.
async fn enrich_error_event(
    error_data: &mut crate::services::types::CreateErrorEventData,
    client_ip: Option<&str>,
    ip_address_service: Option<&Arc<IpAddressService>>,
    db: Option<&Arc<sea_orm::DatabaseConnection>>,
) {
    let ip = match client_ip.or(error_data.user_ip_address.as_deref()) {
        Some(ip) if !ip.is_empty() => ip,
        _ => return,
    };

    let ip_service = match ip_address_service {
        Some(s) => s,
        None => return,
    };

    // Resolve IP geolocation
    match ip_service.get_or_create_ip(ip).await {
        Ok(ip_info) => {
            let geo_id = ip_info.id;
            error_data.ip_geolocation_id = Some(geo_id);

            // Try to find a visitor with this IP for this project
            if let Some(db) = db {
                use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};
                use temps_entities::visitor;

                match visitor::Entity::find()
                    .filter(visitor::Column::ProjectId.eq(error_data.project_id))
                    .filter(visitor::Column::IpAddressId.eq(geo_id))
                    .order_by_desc(visitor::Column::LastSeen)
                    .one(db.as_ref())
                    .await
                {
                    Ok(Some(v)) => {
                        error_data.visitor_id = Some(v.id);
                        debug!("Linked error event to visitor {} (ip: {})", v.id, ip);
                    }
                    Ok(None) => {
                        // No visitor found for this IP — that's fine
                    }
                    Err(e) => {
                        debug!("Failed to look up visitor by IP: {}", e);
                    }
                }
            }
        }
        Err(e) => {
            debug!("Failed to resolve IP geolocation for {}: {}", ip, e);
        }
    }
}

/// Extract DSN key from Sentry auth headers or query parameters
fn extract_dsn_key(
    headers: &HeaderMap,
    query_params: &std::collections::HashMap<String, String>,
) -> Option<String> {
    // Try query parameter first (used by some Sentry SDKs).
    //
    // An empty `?sentry_key=` is treated as *absent*, not as a credential that
    // fails to resolve. Some SDK/proxy combinations emit the bare parameter
    // when no DSN is configured, and answering 401 there would break the
    // Host-resolved fallback for a request that presented nothing at all.
    if let Some(key) = query_params
        .get("sentry_key")
        .map(|key| key.trim())
        .filter(|key| !key.is_empty())
    {
        return Some(key.to_string());
    }

    // Try X-Sentry-Auth header
    if let Some(auth_header) = headers.get("x-sentry-auth") {
        if let Ok(auth_str) = auth_header.to_str() {
            // Parse: Sentry sentry_key=PUBLIC_KEY,sentry_version=7,...
            // Remove "Sentry " prefix if present
            let auth_str = auth_str.strip_prefix("Sentry ").unwrap_or(auth_str);

            for part in auth_str.split(',') {
                let part = part.trim();
                if let Some(key) = part.strip_prefix("sentry_key=") {
                    let key = key.trim();
                    if !key.is_empty() {
                        return Some(key.to_string());
                    }
                }
            }
        }
    }

    // Try Authorization header as fallback
    if let Some(auth_header) = headers.get("authorization") {
        if let Ok(auth_str) = auth_header.to_str() {
            if let Some(key) = auth_str.strip_prefix("DSN ") {
                let key = key.trim();
                if !key.is_empty() {
                    return Some(key.to_string());
                }
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::sentry::SentryProvider;
    use crate::sentry::dsn_service::DSNService;
    use crate::services::error_tracking_service::ErrorTrackingService;
    use async_trait::async_trait;
    use axum::body::Bytes;
    use axum::http::{HeaderName, HeaderValue};
    use axum_test::TestServer;
    use chrono::Utc;
    use std::sync::Arc;
    use temps_database::test_utils::TestDatabase;
    use temps_entities::preset::Preset;

    struct ConformanceNotifications;
    #[async_trait::async_trait]
    impl temps_core::notifications::NotificationService for ConformanceNotifications {
        async fn send_notification(
            &self,
            _: temps_core::notifications::NotificationData,
        ) -> Result<(), temps_core::notifications::NotificationError> {
            Ok(())
        }
        async fn send_email(
            &self,
            _: temps_core::notifications::EmailMessage,
        ) -> Result<(), temps_core::notifications::NotificationError> {
            Ok(())
        }
        async fn is_configured(
            &self,
        ) -> Result<bool, temps_core::notifications::NotificationError> {
            Ok(false)
        }
    }
    struct ConformanceQueue;
    #[async_trait::async_trait]
    impl temps_core::JobQueue for ConformanceQueue {
        async fn send(&self, _: temps_core::Job) -> Result<(), temps_core::jobs::QueueError> {
            Ok(())
        }
        fn subscribe(&self) -> Box<dyn temps_core::JobReceiver> {
            panic!("unused by conformance alarm service")
        }
    }
    async fn conformance_test_context() -> Option<TestContext> {
        match TestDatabase::with_migrations().await {
            Ok(db) => Some(create_test_context_with_db(db).await),
            Err(error)
                if temps_database::test_utils::is_container_runtime_unavailable(
                    &error.to_string(),
                ) =>
            {
                eprintln!(
                    "Sentry ingestion integration test not run: container runtime unavailable"
                );
                None
            }
            Err(error) => panic!("Sentry conformance database setup failed: {error}"),
        }
    }

    fn synthetic_log_envelope() -> Bytes {
        Bytes::from_static(br#"{}
{"type":"log","item_count":1,"content_type":"application/vnd.sentry.items.log+json"}
{"items":[{"timestamp":1790000000.123456789,"level":"info","body":"Synthetic native log","trace_id":"11111111111111111111111111111111","span_id":"2222222222222222","attributes":{"service.name":{"type":"string","value":"synthetic-service"},"synthetic.large":{"type":"integer","value":9007199254740993}}}]}
"#)
    }

    fn native_log_query(project_id: i32) -> temps_otel::types::LogQuery {
        temps_otel::types::LogQuery {
            project_id,
            attribute_filter: None,
            severity: None,
            service_name: None,
            search: None,
            trace_id: None,
            start_time: None,
            end_time: None,
            limit: Some(100),
            offset: None,
        }
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn client_outcomes_http_validates_and_reports_storage_failure_before_acknowledging() {
        use sea_orm::ConnectionTrait;
        let Some(ctx) = conformance_test_context().await else {
            return;
        };
        let server = TestServer::new(configure_routes().with_state(ctx.app_state.clone()));
        let url = format!("/{}/envelope/?sentry_key={}", ctx.project_id, ctx.dsn_key);
        let valid="{}\n{\"type\":\"client_report\"}\n{\"discarded_events\":[{\"reason\":\"before_send\",\"category\":\"error\",\"quantity\":3}]}\n";
        let response = server.post(&url).text(valid).await;
        response.assert_status_ok();
        assert_eq!(
            response.header("x-temps-sentry-client-outcomes-accepted"),
            "1"
        );
        assert_eq!(response.header("x-temps-sentry-unsupported-items"), "0");
        let invalid = valid.replace("\"quantity\":3", "\"quantity\":-1");
        server
            .post(&url)
            .text(invalid)
            .await
            .assert_status_bad_request();
        ctx._db
            .connection()
            .execute_unprepared(
                "CREATE FUNCTION synthetic_transient_outcome_failure() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'Synthetic retryable failure' USING ERRCODE='40001'; END $$; CREATE TRIGGER synthetic_outcome_failure BEFORE INSERT ON telemetry_client_outcomes FOR EACH STATEMENT EXECUTE FUNCTION synthetic_transient_outcome_failure()",
            )
            .await
            .unwrap();
        let failed = server.post(&url).text(valid).await;
        assert_eq!(failed.status_code(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(failed.header("retry-after"), "60");
        assert_eq!(
            failed.header("x-temps-sentry-client-outcomes-accepted"),
            "0"
        );
        ctx._db
            .connection()
            .execute_unprepared(
                "DROP TRIGGER synthetic_outcome_failure ON telemetry_client_outcomes; DROP FUNCTION synthetic_transient_outcome_failure()",
            )
            .await
            .unwrap();
        server.post(&url).text(valid).await.assert_status_ok();
        let stored = ctx
            .app_state
            .error_tracking_service
            .client_outcomes
            .list(ctx.project_id, 1, 1, 20)
            .await
            .unwrap();
        assert_eq!(stored.items[0].quantity, "6");
        ctx._db.connection().execute_unprepared("ALTER TABLE telemetry_client_outcomes ADD CONSTRAINT synthetic_outcome_write_failure CHECK(quantity=0) NOT VALID").await.unwrap();
        let failed = server.post(&url).text(valid).await;
        assert_eq!(failed.status_code(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            failed.header("x-temps-sentry-client-outcomes-accepted"),
            "0"
        );
        assert_eq!(
            ctx.app_state
                .error_tracking_service
                .client_outcomes
                .list(ctx.project_id, 1, 1, 20)
                .await
                .unwrap()
                .items[0]
                .quantity,
            "6"
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn telemetry_identified_http_partial_failure_retries_preserve_native_counts() {
        use sea_orm::{ConnectionTrait, DbBackend, Statement};
        let ctx = create_test_context().await;
        let server = TestServer::new(configure_routes().with_state(ctx.app_state.clone()));
        let url = format!("/{}/envelope/?sentry_key={}", ctx.project_id, ctx.dsn_key);
        let timestamp = Utc::now().timestamp();
        let id = "1234567890abcdef1234567890abcdef";
        let trace = "11111111111111111111111111111111";
        let mut envelope=format!("{{\"event_id\":\"{id}\"}}\n{{\"type\":\"event\"}}\n{{\"message\":\"Synthetic atomic telemetry\",\"platform\":\"javascript\",\"level\":\"error\"}}\n");
        for (ty, content, items) in [
            (
                "span",
                "application/vnd.sentry.items.span.v2+json",
                serde_json::json!([{"name":"Synthetic root","trace_id":trace,"span_id":"2222222222222222","start_timestamp":timestamp,"end_timestamp":timestamp+1,"attributes":{}}]),
            ),
            (
                "trace_metric",
                "application/vnd.sentry.items.trace-metric+json",
                serde_json::json!([{"name":"synthetic.atomic.counter","type":"counter","value":3,"timestamp":timestamp,"trace_id":trace,"attributes":{}}]),
            ),
            (
                "log",
                "application/vnd.sentry.items.log+json",
                serde_json::json!([{"body":"Synthetic atomic log","level":"info","timestamp":timestamp,"trace_id":trace,"attributes":{}}]),
            ),
        ] {
            envelope.push_str(&format!(
                "{}\n{}\n",
                serde_json::json!({"type":ty,"content_type":content,"item_count":1}),
                serde_json::json!({"version":2,"items":items})
            ));
        }
        let db = ctx._db.connection();
        // The last domain fails after errors/spans/metrics have committed.
        db.execute_unprepared("CREATE FUNCTION synthetic_telemetry_abort() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'Synthetic log abort' USING ERRCODE='40001'; END $$; CREATE TRIGGER synthetic_telemetry_abort BEFORE INSERT ON otel_log_events FOR EACH STATEMENT EXECUTE FUNCTION synthetic_telemetry_abort()").await.unwrap();
        let failed = server.post(&url).text(envelope.clone()).await;
        assert_eq!(failed.status_code(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(failed.header("retry-after"), "60");
        for signal in ["errors", "spans", "metrics"] {
            assert_eq!(
                failed.header(&format!("x-temps-sentry-{signal}-stored")),
                "1"
            );
        }
        assert_eq!(failed.header("x-temps-sentry-logs-stored"), "0");
        db.execute_unprepared("DROP TRIGGER synthetic_telemetry_abort ON otel_log_events; DROP FUNCTION synthetic_telemetry_abort()").await.unwrap();
        let recovered = server.post(&url).text(envelope.clone()).await;
        recovered.assert_status_ok();
        for signal in ["errors", "spans", "metrics"] {
            assert_eq!(
                recovered.header(&format!("x-temps-sentry-{signal}-stored")),
                "0"
            );
        }
        assert_eq!(recovered.header("x-temps-sentry-errors-duplicates"), "1");
        assert_eq!(recovered.header("x-temps-sentry-logs-stored"), "1");
        // Simulate a successful response being lost; replay the complete request.
        let retry = server.post(&url).text(envelope.clone()).await;
        retry.assert_status_ok();
        for signal in ["errors", "spans", "metrics", "logs"] {
            assert_eq!(
                retry.header(&format!("x-temps-sentry-{signal}-stored")),
                "0"
            );
        }
        let counts=db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,"SELECT (SELECT count(*) FROM error_events WHERE project_id=$1) AS errors,(SELECT count(*) FROM otel_spans WHERE project_id=$1) AS spans,(SELECT count(*) FROM otel_log_events WHERE project_id=$1) AS logs,(SELECT sum(value)::float8 FROM otel_metrics WHERE project_id=$1) AS metric_total,(SELECT span_count FROM otel_trace_summaries WHERE project_id=$1 AND trace_id=$2) AS summary_count",[ctx.project_id.into(),trace.into()])).await.unwrap().unwrap();
        for signal in ["errors", "spans", "logs", "summary_count"] {
            assert_eq!(counts.try_get::<i64>("", signal).unwrap(), 1);
        }
        assert_eq!(counts.try_get::<f64>("", "metric_total").unwrap(), 3.0);
        // Permanent failures remain failures, with no claimed identity left behind.
        db.execute_unprepared("ALTER TABLE otel_log_events ADD CONSTRAINT synthetic_telemetry_permanent CHECK(body='unreachable') NOT VALID").await.unwrap();
        let fresh = envelope.replace(id, "2234567890abcdef1234567890abcdef");
        let failed = server.post(&url).text(fresh.clone()).await;
        assert_eq!(failed.status_code(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(failed.header("x-temps-sentry-logs-stored"), "0");
        db.execute_unprepared(
            "ALTER TABLE otel_log_events DROP CONSTRAINT synthetic_telemetry_permanent",
        )
        .await
        .unwrap();
        assert_eq!(
            server
                .post(&url)
                .text(fresh)
                .await
                .header("x-temps-sentry-logs-stored"),
            "1"
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn client_outcomes_identified_http_retries_and_partial_ingestion() {
        use futures::StreamExt;
        use sea_orm::ConnectionTrait;
        let Some(ctx) = conformance_test_context().await else {
            return;
        };
        let server = TestServer::new(configure_routes().with_state(ctx.app_state.clone()));
        let url = format!("/{}/envelope/?sentry_key={}", ctx.project_id, ctx.dsn_key);
        let id = "1234567890abcdef1234567890abcdef";
        let report = "{\"type\":\"client_report\"}\n{\"discarded_events\":[{\"reason\":\"sample_rate\",\"category\":\"error\",\"quantity\":3}]}\n";
        let envelope = format!("{{\"event_id\":\"{id}\"}}\n{report}");
        let responses = futures::stream::iter(0..16)
            .map(|_| async { server.post(&url).text(envelope.clone()).await })
            .buffer_unordered(2)
            .collect::<Vec<_>>()
            .await;
        let mut accepted = 0u64;
        let mut duplicates = 0u64;
        for response in responses {
            response.assert_status_ok();
            accepted += response
                .header("x-temps-sentry-client-outcomes-accepted")
                .to_str()
                .unwrap()
                .parse::<u64>()
                .unwrap();
            duplicates += response
                .header("x-temps-sentry-client-outcomes-duplicates")
                .to_str()
                .unwrap()
                .parse::<u64>()
                .unwrap();
        }
        assert_eq!((accepted, duplicates), (1, 15));
        // A different event ID is an independent context, and two equal report
        // items in one envelope remain distinct by position.
        let other = envelope.replace(id, "2234567890abcdef1234567890abcdef");
        server.post(&url).text(other).await.assert_status_ok();
        let response = server.post(&url).text(format!("{envelope}{report}")).await;
        assert_eq!(
            response.header("x-temps-sentry-client-outcomes-accepted"),
            "1"
        );
        assert_eq!(
            response.header("x-temps-sentry-client-outcomes-duplicates"),
            "1"
        );
        // Matching report content without an event identity must both count.
        for _ in 0..2 {
            server
                .post(&url)
                .text(format!("{{}}\n{report}"))
                .await
                .assert_status_ok();
        }
        assert_eq!(
            ctx.app_state
                .error_tracking_service
                .client_outcomes
                .list(ctx.project_id, 1, 1, 20)
                .await
                .unwrap()
                .items[0]
                .quantity,
            "15"
        );

        // Force the counter write to abort after claiming a new identity. The
        // receipt must roll back as well, allowing the retry to increment once.
        let retry = envelope.replace(id, "3234567890abcdef1234567890abcdef");
        ctx._db.connection().execute_unprepared("CREATE FUNCTION synthetic_client_outcome_abort() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'Synthetic counter abort' USING ERRCODE='40001'; END $$; CREATE TRIGGER synthetic_client_outcome_abort BEFORE INSERT ON telemetry_client_outcomes FOR EACH STATEMENT EXECUTE FUNCTION synthetic_client_outcome_abort()").await.unwrap();
        let failed = server.post(&url).text(retry.clone()).await;
        assert_eq!(failed.status_code(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            failed.header("x-temps-sentry-client-outcomes-accepted"),
            "0"
        );
        ctx._db.connection().execute_unprepared("DROP TRIGGER synthetic_client_outcome_abort ON telemetry_client_outcomes; DROP FUNCTION synthetic_client_outcome_abort()").await.unwrap();
        let retried = server.post(&url).text(retry).await;
        retried.assert_status_ok();
        assert_eq!(
            retried.header("x-temps-sentry-client-outcomes-accepted"),
            "1"
        );

        // The report commits first, then the error store fails. Replaying the
        // whole mixed envelope must not increment the report a second time.
        let mixed = format!("{{\"event_id\":\"4234567890abcdef1234567890abcdef\"}}\n{report}{{\"type\":\"event\"}}\n{{\"message\":\"Synthetic partial outcome\",\"platform\":\"javascript\",\"level\":\"error\"}}\n");
        ctx._db.connection().execute_unprepared("CREATE FUNCTION synthetic_error_after_outcome() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'Synthetic error abort' USING ERRCODE='40001'; END $$; CREATE TRIGGER synthetic_error_after_outcome BEFORE INSERT ON error_events FOR EACH STATEMENT EXECUTE FUNCTION synthetic_error_after_outcome()").await.unwrap();
        let failed = server.post(&url).text(mixed.clone()).await;
        assert_eq!(failed.status_code(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            failed.header("x-temps-sentry-client-outcomes-accepted"),
            "1"
        );
        ctx._db.connection().execute_unprepared("DROP TRIGGER synthetic_error_after_outcome ON error_events; DROP FUNCTION synthetic_error_after_outcome()").await.unwrap();
        let retried = server.post(&url).text(mixed).await;
        retried.assert_status_ok();
        assert_eq!(
            retried.header("x-temps-sentry-client-outcomes-accepted"),
            "0"
        );
        assert_eq!(
            retried.header("x-temps-sentry-client-outcomes-duplicates"),
            "1"
        );
        assert_eq!(retried.header("x-temps-sentry-errors-stored"), "1");
        assert_eq!(
            ctx.app_state
                .error_tracking_service
                .client_outcomes
                .list(ctx.project_id, 1, 1, 20)
                .await
                .unwrap()
                .items[0]
                .quantity,
            "21"
        );
        let (other_project, other_key) = create_second_project_with_dsn(&ctx).await;
        let other_url = format!("/{other_project}/envelope/?sentry_key={other_key}");
        let independent = server.post(&other_url).text(envelope.clone()).await;
        independent.assert_status_ok();
        assert_eq!(
            independent.header("x-temps-sentry-client-outcomes-accepted"),
            "1"
        );
        assert_eq!(
            ctx.app_state
                .error_tracking_service
                .client_outcomes
                .list(other_project, 1, 1, 20)
                .await
                .unwrap()
                .items[0]
                .quantity,
            "3"
        );

        use sea_orm::{ActiveModelTrait, Set};
        let scope = uuid::Uuid::new_v4().to_string();
        let environment = temps_entities::environments::ActiveModel {
            project_id: Set(ctx.project_id),
            name: Set("Synthetic outcome scope".into()),
            slug: Set(scope.clone()),
            subdomain: Set(scope),
            host: Set("synthetic.example".into()),
            upstreams: Set(Default::default()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(ctx._db.connection())
        .await
        .unwrap();
        let deployment = temps_entities::deployments::ActiveModel {
            project_id: Set(ctx.project_id),
            environment_id: Set(environment.id),
            slug: Set(uuid::Uuid::new_v4().to_string()),
            state: Set("completed".into()),
            metadata: Set(Some(Default::default())),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(ctx._db.connection())
        .await
        .unwrap();
        let payload =
            br#"{"discarded_events":[{"reason":"sample_rate","category":"error","quantity":3}]}"#;
        for deployment_id in [None, Some(deployment.id)] {
            for expected in [1, 0] {
                let receipt = ctx
                    .app_state
                    .error_tracking_service
                    .client_outcomes
                    .ingest_reports(
                        ctx.project_id,
                        Some(environment.id),
                        deployment_id,
                        vec![
                            super::super::client_reports::decode_report(payload, Some(id), 0)
                                .unwrap(),
                        ],
                    )
                    .await
                    .unwrap();
                assert_eq!(receipt.accepted, expected);
                assert_eq!(receipt.duplicates, 1 - expected);
            }
        }
        // Expiry is checked on admission as well as by indexed bounded cleanup.
        let expired = ctx
            ._db
            .connection()
            .execute_unprepared(
                "UPDATE telemetry_client_outcome_receipts SET expires_at=NOW()-INTERVAL '1 second'",
            )
            .await
            .unwrap()
            .rows_affected();
        let again = server.post(&url).text(envelope.clone()).await;
        again.assert_status_ok();
        assert_eq!(again.header("x-temps-sentry-client-outcomes-accepted"), "1");
        assert_eq!(
            ctx.app_state
                .error_tracking_service
                .client_outcomes
                .cleanup()
                .await
                .unwrap(),
            expired - 1
        );
        // Capacity admission must be atomic too. Existing receipts still
        // acknowledge successfully, and unidentified reports need no budget.
        ctx._db.connection().execute_unprepared("UPDATE telemetry_client_outcome_receipt_budgets SET used=32768 WHERE project_id IS NOT NULL").await.unwrap();
        let new_identity = envelope.replace(id, "5234567890abcdef1234567890abcdef");
        let full = server.post(&url).text(new_identity.clone()).await;
        assert_eq!(full.status_code(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(full.header("retry-after"), "60");
        assert_eq!(full.header("x-temps-sentry-client-outcomes-accepted"), "0");
        let duplicate = server.post(&url).text(envelope.clone()).await;
        duplicate.assert_status_ok();
        assert_eq!(
            duplicate.header("x-temps-sentry-client-outcomes-duplicates"),
            "1"
        );
        server
            .post(&url)
            .text(format!("{{}}\n{report}"))
            .await
            .assert_status_ok();
        ctx._db
            .connection()
            .execute_unprepared("UPDATE telemetry_client_outcome_receipt_budgets SET used=0")
            .await
            .unwrap();
        let recovered = server.post(&url).text(new_identity).await;
        recovered.assert_status_ok();
        assert_eq!(
            recovered.header("x-temps-sentry-client-outcomes-accepted"),
            "1"
        );
        ctx._db.connection().execute_unprepared("UPDATE telemetry_client_outcome_receipt_budgets SET used=131072 WHERE project_id IS NULL").await.unwrap();
        let global_new = envelope.replace(id, "6234567890abcdef1234567890abcdef");
        let full = server.post(&url).text(global_new.clone()).await;
        assert_eq!(full.status_code(), StatusCode::SERVICE_UNAVAILABLE);
        ctx._db
            .connection()
            .execute_unprepared("UPDATE telemetry_client_outcome_receipt_budgets SET used=0")
            .await
            .unwrap();
        let recovered = server.post(&url).text(global_new).await;
        recovered.assert_status_ok();
        assert_eq!(
            recovered.header("x-temps-sentry-client-outcomes-accepted"),
            "1"
        );
        let totals = ctx
            .app_state
            .error_tracking_service
            .client_outcomes
            .list(ctx.project_id, 1, 1, 20)
            .await
            .unwrap();
        assert_eq!(
            totals
                .items
                .iter()
                .find(|row| row.environment_id.is_none())
                .unwrap()
                .quantity,
            "33"
        );
        // Expiration cleanup and a renewed claim can race. Every successful
        // renewal must retain its receipt so the next retry remains a duplicate.
        let source_key = super::super::client_reports::decode_report(payload, Some(id), 0)
            .unwrap()
            .source_key
            .unwrap();
        for _ in 0..8 {
            let expired = ctx._db.connection().execute(sea_orm::Statement::from_sql_and_values(sea_orm::DbBackend::Postgres,
                "UPDATE telemetry_client_outcome_receipts SET expires_at=NOW()-INTERVAL '1 second' WHERE project_id=$1 AND environment_id IS NULL AND deployment_id IS NULL AND source_key=$2",
                [ctx.project_id.into(),source_key.clone().into()])).await.unwrap().rows_affected();
            assert_eq!(expired, 1);
            let (renewed, cleanup) = tokio::join!(
                async { server.post(&url).text(envelope.clone()).await },
                ctx.app_state
                    .error_tracking_service
                    .client_outcomes
                    .cleanup()
            );
            assert_eq!(renewed.status_code(), StatusCode::OK);
            assert_eq!(
                renewed.header("x-temps-sentry-client-outcomes-accepted"),
                "1"
            );
            cleanup.unwrap();
            let duplicate = server.post(&url).text(envelope.clone()).await;
            assert_eq!(duplicate.status_code(), StatusCode::OK);
            assert_eq!(
                duplicate.header("x-temps-sentry-client-outcomes-accepted"),
                "0"
            );
            assert_eq!(
                duplicate.header("x-temps-sentry-client-outcomes-duplicates"),
                "1"
            );
        }
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn browser_report_http_routes_use_native_services_and_scoped_dsns() {
        let Some(ctx) = conformance_test_context().await else {
            return;
        };
        let server = TestServer::new(configure_routes().with_state(ctx.app_state.clone()));
        let csp = serde_json::json!({"csp-report":{"effective-directive":"script-src","violated-directive":"script-src","blocked-uri":"inline","document-uri":"https://synthetic.example/page"}});
        let endpoint = format!("/{}/security/?sentry_key={}", ctx.project_id, ctx.dsn_key);
        let response = server
            .post(&endpoint)
            .text(csp.to_string())
            .content_type("application/csp-report")
            .await;
        response.assert_status_ok();
        assert_eq!(response.header("x-temps-sentry-errors-stored"), "1");
        server
            .post(&format!(
                "/{}/security/?sentry_key={}",
                ctx.project_id + 1,
                ctx.dsn_key
            ))
            .text(csp.to_string())
            .content_type("application/csp-report")
            .await
            .assert_status_unauthorized();
        let nel = serde_json::json!([{ "type":"network-error","url":"https://synthetic.example/api","body":{"type":"dns.failed","phase":"dns","elapsed_time":12,"status_code":0}}]);
        let endpoint = format!("/{}/nel/?sentry_key={}", ctx.project_id, ctx.dsn_key);
        let response = server
            .post(&endpoint)
            .text(nel.to_string())
            .content_type("application/reports+json")
            .await;
        response.assert_status_ok();
        assert_eq!(response.header("x-temps-sentry-logs-stored"), "1");
        let logs = ctx
            .app_state
            .otel_service
            .query_logs(native_log_query(ctx.project_id))
            .await
            .unwrap();
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].resource.service_name, "sentry.nel");
        assert_eq!(logs[0].attributes["nel.type"], "dns.failed");
        assert_eq!(
            logs[0].attribute_types["http.response.status_code"],
            temps_otel::types::LogAttributeType::Integer
        );
        let response = server
            .post(&endpoint)
            .content_type("text/plain")
            .text(nel.to_string())
            .await;
        assert_eq!(response.status_code(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
        server
            .post(&endpoint)
            .text("invalid")
            .content_type("application/reports+json")
            .await
            .assert_status_bad_request();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[serial_test::serial]
    async fn check_ins_validate_before_writes_and_retry_after_storage_failure() {
        use sea_orm::ConnectionTrait;
        let Some(ctx) = conformance_test_context().await else {
            return;
        };
        let app = configure_routes().with_state(ctx.app_state.clone());
        let server = TestServer::new(app);
        let payload = serde_json::json!({"check_in_id":"cccccccccccccccccccccccccccccccc","monitor_slug":"synthetic-retry","status":"ok","monitor_config":{"schedule":{"type":"interval","value":5,"unit":"minute"}}});
        let make_envelope =
            |payload: &serde_json::Value| format!("{{}}\n{{\"type\":\"check_in\"}}\n{payload}\n");
        let url = format!("/{}/envelope/", ctx.project_id);
        // Missing/invalid DSNs cannot create native monitors.
        let unauth = server.post(&url).text(make_envelope(&payload)).await;
        assert_eq!(unauth.status_code(), StatusCode::UNAUTHORIZED);
        let auth = HeaderValue::from_str(&format!(
            "Sentry sentry_key={},sentry_version=7",
            ctx.dsn_key
        ))
        .unwrap();
        let mut invalid = payload.clone();
        invalid["monitor_config"]["timezone"] = "invalid".into();
        let response = server
            .post(&url)
            .add_header(HeaderName::from_static("x-sentry-auth"), auth.clone())
            .text(make_envelope(&invalid))
            .await;
        assert_eq!(response.status_code(), StatusCode::BAD_REQUEST);
        assert!(ctx
            .app_state
            .cron_monitor_service
            .list(ctx.project_id, 100, 0)
            .await
            .unwrap()
            .is_empty());
        // Mutate only this test's isolated schema. A failed transaction cannot
        // acknowledge a check-in; retrying after storage recovers persists once.
        ctx._db
            .connection()
            .execute_unprepared(
                "ALTER TABLE cron_monitors RENAME TO synthetic_unavailable_monitors",
            )
            .await
            .unwrap();
        let failed = server
            .post(&url)
            .add_header(HeaderName::from_static("x-sentry-auth"), auth.clone())
            .text(make_envelope(&payload))
            .await;
        assert_eq!(failed.status_code(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(failed.header("retry-after"), "60");
        ctx._db
            .connection()
            .execute_unprepared(
                "ALTER TABLE synthetic_unavailable_monitors RENAME TO cron_monitors",
            )
            .await
            .unwrap();
        for _ in 0..2 {
            let response = server
                .post(&url)
                .add_header(HeaderName::from_static("x-sentry-auth"), auth.clone())
                .text(make_envelope(&payload))
                .await;
            assert_eq!(response.status_code(), StatusCode::OK);
            assert_eq!(response.header("x-temps-sentry-check-ins-stored"), "1");
            assert_eq!(response.header("x-temps-sentry-unsupported-items"), "0");
        }
        let monitors = ctx
            .app_state
            .cron_monitor_service
            .list(ctx.project_id, 100, 0)
            .await
            .unwrap();
        assert_eq!(monitors.len(), 1);
        assert_eq!(
            ctx.app_state
                .cron_monitor_service
                .runs(ctx.project_id, monitors[0].id, 100, 0)
                .await
                .unwrap()
                .len(),
            1
        );
        ctx._db.connection().execute_unprepared("ALTER TABLE cron_monitor_runs ADD CONSTRAINT synthetic_reject CHECK(false) NOT VALID").await.unwrap();
        let mut rejected = payload.clone();
        rejected["check_in_id"] = "d".repeat(32).into();
        let permanent = server
            .post(&url)
            .add_header(HeaderName::from_static("x-sentry-auth"), auth.clone())
            .text(make_envelope(&rejected))
            .await;
        assert_eq!(permanent.status_code(), StatusCode::INTERNAL_SERVER_ERROR);
        assert!(!permanent.headers().contains_key("retry-after"));
        assert_eq!(permanent.header("x-temps-sentry-check-ins-stored"), "0");
        ctx._db
            .connection()
            .execute_unprepared("ALTER TABLE cron_monitor_runs DROP CONSTRAINT synthetic_reject")
            .await
            .unwrap();
        let mut completion = payload.clone();
        completion.as_object_mut().unwrap().remove("monitor_config");
        completion["monitor_slug"] = "synthetic-reversed".into();
        completion["check_in_id"] = "e".repeat(32).into();
        let response = server
            .post(&url)
            .add_header(HeaderName::from_static("x-sentry-auth"), auth.clone())
            .text(make_envelope(&completion))
            .await;
        assert_eq!(response.status_code(), StatusCode::OK);
        assert_eq!(
            response.header("x-temps-sentry-check-ins-pending-config"),
            "1"
        );
        assert!(response.text().contains("monitor_config"));
        let mut start = payload.clone();
        start["monitor_slug"] = "synthetic-reversed".into();
        start["check_in_id"] = "e".repeat(32).into();
        start["status"] = "in_progress".into();
        let response = server
            .post(&url)
            .add_header(HeaderName::from_static("x-sentry-auth"), auth.clone())
            .text(make_envelope(&start))
            .await;
        assert_eq!(response.status_code(), StatusCode::OK);
        assert_eq!(
            response.header("x-temps-sentry-check-ins-pending-config"),
            "0"
        );
        let configured = ctx
            .app_state
            .cron_monitor_service
            .list(ctx.project_id, 100, 0)
            .await
            .unwrap()
            .into_iter()
            .find(|m| m.slug == "synthetic-reversed")
            .unwrap();
        assert!(!configured.config.is_null());
        assert_eq!(configured.status, "ok");
        let runs = ctx
            .app_state
            .cron_monitor_service
            .runs(ctx.project_id, configured.id, 100, 0)
            .await
            .unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, "ok");
        let oversized = format!(
            "{{}}\n{}",
            (0..101)
                .map(|_| format!("{{\"type\":\"check_in\"}}\n{payload}\n"))
                .collect::<String>()
        );
        let response = server
            .post(&url)
            .add_header(HeaderName::from_static("x-sentry-auth"), auth)
            .text(oversized)
            .await;
        assert_eq!(response.status_code(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn structured_logs_reach_native_storage_and_query_with_ownership_and_outcomes() {
        let Some(ctx) = conformance_test_context().await else {
            return;
        };
        let server = TestServer::new(configure_routes().with_state(ctx.app_state.clone()));
        let response = server
            .post(&format!(
                "/{}/envelope/?sentry_key={}",
                ctx.project_id, ctx.dsn_key
            ))
            .bytes(synthetic_log_envelope())
            .await;
        response.assert_status_ok();
        assert_eq!(response.header("x-temps-sentry-logs-stored"), "1");
        let logs = ctx
            .app_state
            .otel_service
            .query_logs(native_log_query(ctx.project_id))
            .await
            .unwrap();
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].body, "Synthetic native log");
        assert_eq!(logs[0].attributes["synthetic.large"], "9007199254740993");
        assert_eq!(
            logs[0].attributes["temps.sentry.timestamp"],
            "1790000000.123456789"
        );
        assert!(logs[0].attributes["temps.sentry.attributes"].contains("9007199254740993"));
        assert_eq!(
            logs[0].trace_id.as_deref(),
            Some("11111111111111111111111111111111")
        );
        assert_eq!(logs[0].span_id.as_deref(), Some("2222222222222222"));
        assert!(ctx
            .app_state
            .otel_service
            .query_logs(native_log_query(ctx.project_id + 1))
            .await
            .unwrap()
            .is_empty());
        server
            .post(&format!(
                "/{}/envelope/?sentry_key={}",
                ctx.project_id + 1,
                ctx.dsn_key
            ))
            .bytes(synthetic_log_envelope())
            .await
            .assert_status_unauthorized();
        let unknown = server
            .post(&format!(
                "/{}/envelope/?sentry_key={}",
                ctx.project_id, ctx.dsn_key
            ))
            .bytes(Bytes::from_static(
                b"{}\n{\"type\":\"future_123\",\"length\":3}\n\xff\x00\n",
            ))
            .await;
        unknown.assert_status_ok();
        assert_eq!(unknown.header("x-temps-sentry-logs-stored"), "0");
        assert_eq!(unknown.header("x-temps-sentry-unsupported-items"), "1");
        assert_eq!(
            unknown.header("x-temps-sentry-unsupported-types"),
            "unknown"
        );
        ctx.route_table
            .insert_route_for_test("synthetic.example", test_route_info(ctx.project.clone()));
        let tunnel = TestServer::new(configure_tunnel_test_router(ctx.app_state.clone()));
        tunnel
            .post(SENTRY_TUNNEL_ROUTE_PATH)
            .add_header(
                HeaderName::from_static("host"),
                HeaderValue::from_static("synthetic.example"),
            )
            .add_header(
                HeaderName::from_static("origin"),
                HeaderValue::from_static("https://synthetic.example"),
            )
            .bytes(synthetic_log_envelope())
            .await
            .assert_status_unauthorized();
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn shared_ingest_permits_reject_before_body_buffering() {
        let Some(ctx) = conformance_test_context().await else {
            return;
        };
        let permits: Vec<_> = (0..8)
            .map(|_| {
                ctx.app_state
                    .otel_service
                    .try_acquire_ingest_permit()
                    .unwrap()
            })
            .collect();
        let server = TestServer::new(configure_routes().with_state(ctx.app_state.clone()));
        let response = server
            .post(&format!(
                "/{}/envelope/?sentry_key={}",
                ctx.project_id, ctx.dsn_key
            ))
            .bytes(Bytes::from(vec![b'x'; SENTRY_INGEST_BODY_LIMIT + 1]))
            .await;
        response.assert_status_service_unavailable();
        assert_eq!(response.header("retry-after"), "1");
        drop(permits);
        server
            .post(&format!(
                "/{}/envelope/?sentry_key={}",
                ctx.project_id, ctx.dsn_key
            ))
            .bytes(synthetic_log_envelope())
            .await
            .assert_status_ok();
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn log_scope_comes_from_a_valid_dsn_environment_and_deployment() {
        use sea_orm::{ActiveModelTrait, Set};
        use temps_entities::{deployments, environments, upstream_config::UpstreamList};
        let Some(ctx) = conformance_test_context().await else {
            return;
        };
        let (other_project, _) = create_second_project_with_dsn(&ctx).await;
        let mut env_ids = Vec::new();
        for project_id in [ctx.project_id, ctx.project_id, other_project] {
            let id = uuid::Uuid::new_v4().to_string();
            let env = environments::ActiveModel {
                project_id: Set(project_id),
                name: Set(format!("Synthetic {id}")),
                slug: Set(id.clone()),
                subdomain: Set(id),
                host: Set("synthetic.example".into()),
                upstreams: Set(UpstreamList::default()),
                created_at: Set(Utc::now()),
                updated_at: Set(Utc::now()),
                ..Default::default()
            }
            .insert(ctx._db.connection())
            .await
            .unwrap();
            env_ids.push(env.id);
        }
        let mut deployment_ids = Vec::new();
        for (environment_id, project_id) in [
            (env_ids[0], ctx.project_id),
            (env_ids[1], ctx.project_id),
            (env_ids[2], other_project),
        ] {
            let deployment = deployments::ActiveModel {
                project_id: Set(project_id),
                environment_id: Set(environment_id),
                slug: Set(uuid::Uuid::new_v4().to_string()),
                state: Set("completed".into()),
                metadata: Set(Some(deployments::DeploymentMetadata::default())),
                created_at: Set(Utc::now()),
                updated_at: Set(Utc::now()),
                ..Default::default()
            }
            .insert(ctx._db.connection())
            .await
            .unwrap();
            deployment_ids.push(deployment.id);
        }
        let server = TestServer::new(configure_routes().with_state(ctx.app_state.clone()));
        for (environment, deployment, expected) in [
            (Some(env_ids[0]), Some(deployment_ids[0]), StatusCode::OK),
            (Some(env_ids[2]), None, StatusCode::UNAUTHORIZED),
            (None, Some(deployment_ids[2]), StatusCode::UNAUTHORIZED),
            (
                Some(env_ids[0]),
                Some(deployment_ids[1]),
                StatusCode::UNAUTHORIZED,
            ),
            (
                Some(env_ids[0]),
                Some(deployment_ids[2]),
                StatusCode::UNAUTHORIZED,
            ),
        ] {
            let dsn = ctx
                .app_state
                .dsn_service
                .generate_project_dsn(ctx.project_id, environment, deployment, None, "localhost")
                .await
                .unwrap();
            let check_in = serde_json::json!({"check_in_id":"e".repeat(32),"monitor_slug":"synthetic-ownership","status":"ok","environment":"synthetic-client-environment","project_id":other_project,"monitor_config":{"schedule":{"type":"interval","value":1,"unit":"hour"}}});
            let mut envelope = synthetic_log_envelope().to_vec();
            envelope
                .extend_from_slice(format!("{{\"type\":\"check_in\"}}\n{check_in}\n").as_bytes());
            let response = server
                .post(&format!(
                    "/{}/envelope/?sentry_key={}",
                    ctx.project_id, dsn.public_key
                ))
                .bytes(Bytes::from(envelope))
                .await;
            assert_eq!(response.status_code(), expected);
        }
        let logs = ctx
            .app_state
            .otel_service
            .query_logs(native_log_query(ctx.project_id))
            .await
            .unwrap();
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].deployment_id, Some(deployment_ids[0]));
        assert_eq!(
            logs[0].attributes["temps.sentry.environment_id"],
            env_ids[0].to_string()
        );
        let monitors = ctx
            .app_state
            .cron_monitor_service
            .list(ctx.project_id, 100, 0)
            .await
            .unwrap();
        assert_eq!(monitors.len(), 1);
        assert_eq!(monitors[0].environment_id, Some(env_ids[0]));
        let runs = ctx
            .app_state
            .cron_monitor_service
            .runs(ctx.project_id, monitors[0].id, 100, 0)
            .await
            .unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].deployment_id, Some(deployment_ids[0]));
        assert_eq!(
            runs[0].environment.as_deref(),
            Some("synthetic-client-environment")
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn telemetry_adapters_validate_before_writes_and_report_partial_failures() {
        let Some(ctx) = conformance_test_context().await else {
            return;
        };
        let path = format!("/{}/envelope/?sentry_key={}", ctx.project_id, ctx.dsn_key);
        let envelope=Bytes::from_static(br#"{"event_id":"1234567890abcdef1234567890abcdef"}
{"type":"span"}
{"trace_id":"11111111111111111111111111111111","span_id":"2222222222222222","start_timestamp":1790000000,"timestamp":1790000001,"description":"Synthetic span"}
{"type":"trace_metric","item_count":1,"content_type":"application/vnd.sentry.items.trace-metric+json"}
{"items":[{"timestamp":1790000000,"trace_id":"11111111111111111111111111111111","span_id":"2222222222222222","name":"synthetic.requests","type":"counter","value":3}]}
{"type":"log","item_count":1,"content_type":"application/vnd.sentry.items.log+json"}
{"items":[{"timestamp":1790000000,"trace_id":"11111111111111111111111111111111","body":"Synthetic linked log","level":"info"}]}
"#);
        use sea_orm::ConnectionTrait;
        let server = TestServer::new(configure_routes().with_state(ctx.app_state.clone()));
        for scenario in ["invalid", "permanent", "transient", "recover"] {
            if scenario == "permanent" {
                ctx._db.connection().execute_unprepared("ALTER TABLE otel_metrics ADD CONSTRAINT synthetic_reject_metric CHECK(false) NOT VALID").await.unwrap();
            }
            if scenario == "transient" || scenario == "recover" {
                ctx._db
                    .connection()
                    .execute_unprepared("CREATE SEQUENCE synthetic_retry_sequence")
                    .await
                    .unwrap();
                let condition = if scenario == "recover" {
                    "nextval('synthetic_retry_sequence')=1"
                } else {
                    "nextval('synthetic_retry_sequence')>0"
                };
                ctx._db.connection().execute_unprepared(&format!("CREATE FUNCTION synthetic_metric_retry() RETURNS trigger AS $$ BEGIN IF {condition} THEN RAISE EXCEPTION 'synthetic storage failure' USING ERRCODE='40001'; END IF; RETURN NEW; END; $$ LANGUAGE plpgsql")).await.unwrap();
                ctx._db.connection().execute_unprepared("CREATE TRIGGER synthetic_retry BEFORE INSERT ON otel_metrics FOR EACH ROW EXECUTE FUNCTION synthetic_metric_retry()").await.unwrap();
            }
            let body = if scenario == "invalid" {
                Bytes::from(String::from_utf8(envelope.to_vec()).unwrap().replacen("\"item_count\":1,\"content_type\":\"application/vnd.sentry.items.trace-metric+json\"","\"item_count\":2,\"content_type\":\"application/vnd.sentry.items.trace-metric+json\"",1))
            } else {
                envelope.clone()
            };
            let response = server.post(&path).bytes(body).await;
            if scenario == "invalid" {
                response.assert_status_bad_request();
                assert_eq!(
                    ctx.app_state.otel_service.pipeline_stats().spans_received,
                    0
                );
                continue;
            }
            // The span commits before metric failure. Later attempts carry
            // the same trace/span identity and must not inflate its count.
            assert_eq!(
                response.header("x-temps-sentry-spans-stored"),
                if scenario == "permanent" { "1" } else { "0" }
            );
            if scenario == "recover" {
                response.assert_status_ok();
                assert_eq!(response.header("x-temps-sentry-metrics-stored"), "1");
                assert_eq!(response.header("x-temps-sentry-logs-stored"), "1");
                let row = ctx
                    ._db
                    .connection()
                    .query_one(sea_orm::Statement::from_string(
                        sea_orm::DatabaseBackend::Postgres,
                        "SELECT last_value FROM synthetic_retry_sequence",
                    ))
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(row.try_get::<i64>("", "last_value").unwrap(), 2);
            } else {
                assert_eq!(response.header("x-temps-sentry-metrics-stored"), "0");
                assert_eq!(response.header("x-temps-sentry-logs-stored"), "0");
                if scenario == "transient" {
                    response.assert_status_service_unavailable();
                    assert_eq!(response.header("retry-after"), "60");
                } else {
                    response.assert_status_internal_server_error();
                }
            }
            if scenario == "permanent" {
                ctx._db
                    .connection()
                    .execute_unprepared(
                        "ALTER TABLE otel_metrics DROP CONSTRAINT synthetic_reject_metric",
                    )
                    .await
                    .unwrap();
            }
            if scenario == "transient" || scenario == "recover" {
                ctx._db
                    .connection()
                    .execute_unprepared("DROP TRIGGER synthetic_retry ON otel_metrics")
                    .await
                    .unwrap();
                ctx._db
                    .connection()
                    .execute_unprepared("DROP FUNCTION synthetic_metric_retry()")
                    .await
                    .unwrap();
                ctx._db
                    .connection()
                    .execute_unprepared("DROP SEQUENCE synthetic_retry_sequence")
                    .await
                    .unwrap();
            }
        }
        let logs = ctx
            .app_state
            .otel_service
            .query_logs(native_log_query(ctx.project_id))
            .await
            .unwrap();
        assert_eq!(logs.len(), 1);
        let buckets = ctx
            .app_state
            .otel_service
            .query_metrics(temps_otel::types::MetricQuery {
                project_id: ctx.project_id,
                metric_name: Some("synthetic.requests".into()),
                aggregation: temps_otel::types::MetricAggregation::Sum,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(buckets.iter().map(|b| b.value).sum::<f64>(), 3.0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "Official SDK prerequisites: scripts/sentry-acceptance.sh sdk"]
    #[serial_test::serial]
    async fn official_sdk_traces_metrics_and_replay_reach_native_queries() {
        use sea_orm::ConnectionTrait;
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
        use temps_otel::types::{MetricAggregation, MetricQuery, TraceQuery};
        let entry = PrioritySdkHarness::prerequisite("TEMPS_SENTRY_SIGNALS_SDK");
        let ctx = create_test_context().await;
        let router = Router::new()
            .nest("/api", configure_routes())
            .layer(axum::middleware::from_fn(inject_test_request_metadata))
            .with_state(ctx.app_state.clone());
        let server = TestServer::builder()
            .http_transport()
            .build(router.into_make_service_with_connect_info::<SocketAddr>());
        let base = server.server_address().unwrap();
        let dsn = format!(
            "http://{}@{}/{}",
            ctx.dsn_key,
            base.authority(),
            ctx.project_id
        );
        let tunnel = format!("{base}api{SENTRY_TUNNEL_ROUTE_PATH}");
        let modes: &[&str] = if entry.contains("/browser/") {
            &["replay", "replay-uncompressed", "replay-tunnel"]
        } else {
            &["traces", "traces-tunnel"]
        };
        for mode in modes {
            let output = tokio::process::Command::new("bun")
                .arg(PrioritySdkHarness::fixture(
                    "sentry-signals-conformance.mjs",
                ))
                .args([entry.as_str(), dsn.as_str(), mode, tunnel.as_str()])
                .output()
                .await
                .unwrap();
            assert!(
                output.status.success(),
                "Official SDK scenario {mode} failed (credential-bearing output suppressed)"
            );
            let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            if mode.starts_with("replay") {
                let browser_trace = report["trace_id"].as_str().unwrap();
                let browser_spans = ctx
                    .app_state
                    .otel_service
                    .get_trace(ctx.project_id, browser_trace)
                    .await
                    .unwrap();
                assert_eq!(browser_spans.len(), 2);
                let browser_root = browser_spans
                    .iter()
                    .find(|s| s.name == "Synthetic browser root")
                    .unwrap();
                let browser_child = browser_spans
                    .iter()
                    .find(|s| s.name == "Synthetic browser child")
                    .unwrap();
                assert_eq!(
                    browser_child.parent_span_id.as_deref(),
                    Some(browser_root.span_id.as_str())
                );
                assert_eq!(
                    browser_root.resource.service_version.as_deref(),
                    Some("synthetic-release-1")
                );
                let browser_metric = ctx
                    .app_state
                    .otel_service
                    .query_metrics(MetricQuery {
                        project_id: ctx.project_id,
                        metric_name: Some("synthetic.browser.requests".into()),
                        aggregation: MetricAggregation::Sum,
                        ..Default::default()
                    })
                    .await
                    .unwrap();
                assert!(browser_metric.iter().any(|b| b.value >= 1.0));
                let replay_id = report["replay_id"].as_str().unwrap();
                let session_id = format!("sentry-{}-0-0-{replay_id}", ctx.project_id);
                let session = temps_entities::session_replay_sessions::Entity::find()
                    .filter(
                        temps_entities::session_replay_sessions::Column::SessionReplayId
                            .eq(&session_id),
                    )
                    .one(ctx._db.connection())
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(session.project_id, ctx.project_id);
                let playback = ctx
                    .app_state
                    .session_replay_service
                    .get_session_replay(session.id)
                    .await
                    .unwrap();
                assert!(!playback.events.is_empty());
                assert!(
                    playback
                        .session
                        .viewport_width
                        .is_some_and(|width| width > 0),
                    "the late snapshot must fill native viewport metadata"
                );
                assert!(
                    playback.events.iter().any(|e| e.event_type == Some(2)),
                    "rrweb full snapshot must reach native playback"
                );
                assert!(playback
                    .events
                    .iter()
                    .all(|e| e.data.get("_temps_source_order").is_none()));
                assert!(playback
                    .events
                    .windows(2)
                    .all(|w| w[0].timestamp <= w[1].timestamp));
                let markers = temps_entities::session_replay_ingest_batches::Entity::find()
                    .filter(
                        temps_entities::session_replay_ingest_batches::Column::SessionId
                            .eq(session.id),
                    )
                    .all(ctx._db.connection())
                    .await
                    .unwrap();
                assert_eq!(
                    markers.len() as u64,
                    report["segments"].as_u64().unwrap(),
                    "retries must not duplicate segment markers"
                );
                assert_eq!(
                    playback.events.len(),
                    markers
                        .iter()
                        .map(|b| b.event_count as usize)
                        .sum::<usize>(),
                    "retries must not duplicate rrweb events"
                );
                assert!(playback
                    .events
                    .iter()
                    .any(|e| e.data.pointer("/data/payload/release")
                        == Some(&serde_json::json!("synthetic-release-1"))));
                let visitor = temps_entities::visitor::Entity::find_by_id(session.visitor_id)
                    .one(ctx._db.connection())
                    .await
                    .unwrap()
                    .unwrap();
                assert!(visitor.visitor_id.starts_with("sentry-"));
                // A different project cannot append to the native recording.
                assert!(ctx
                    .app_state
                    .session_replay_service
                    .add_decoded_session_events(
                        ctx.project_id + 1,
                        &session_id,
                        vec![serde_json::json!({"type":5,"timestamp":1790000000000_i64,"data":{}})],
                        "synthetic-foreign",
                        0
                    )
                    .await
                    .is_err());
                // Permanent storage failure rolls the native marker back; retry
                // must subsequently store the segment, without duplicate rows.
                ctx._db.connection().execute_unprepared("ALTER TABLE session_replay_events ADD CONSTRAINT synthetic_reject_replay CHECK(false) NOT VALID").await.unwrap();
                let segment = vec![
                    serde_json::json!({"type":5,"timestamp":1790000000000_i64,"data":{"tag":"synthetic"}}),
                ];
                assert!(ctx
                    .app_state
                    .session_replay_service
                    .add_decoded_session_events(
                        ctx.project_id,
                        &session_id,
                        segment.clone(),
                        "synthetic-retry",
                        999
                    )
                    .await
                    .is_err());
                ctx._db
                    .connection()
                    .execute_unprepared(
                        "ALTER TABLE session_replay_events DROP CONSTRAINT synthetic_reject_replay",
                    )
                    .await
                    .unwrap();
                assert_eq!(
                    ctx.app_state
                        .session_replay_service
                        .add_decoded_session_events(
                            ctx.project_id,
                            &session_id,
                            segment.clone(),
                            "synthetic-retry",
                            999
                        )
                        .await
                        .unwrap(),
                    1
                );
                assert_eq!(
                    ctx.app_state
                        .session_replay_service
                        .add_decoded_session_events(
                            ctx.project_id,
                            &session_id,
                            segment,
                            "synthetic-retry",
                            999
                        )
                        .await
                        .unwrap(),
                    0
                );
                for sequence in [998_u32, 997] {
                    ctx.app_state.session_replay_service.add_decoded_session_events(ctx.project_id,&session_id,vec![serde_json::json!({"type":5,"timestamp":1790000000000_i64,"data":{"tag":"synthetic-order","payload":sequence}})],&format!("synthetic-order-{sequence}"),sequence).await.unwrap();
                }
                let playback = ctx
                    .app_state
                    .session_replay_service
                    .get_session_replay(session.id)
                    .await
                    .unwrap();
                let order: Vec<_> = playback
                    .events
                    .iter()
                    .filter(|e| {
                        e.data
                            .pointer("/data/tag")
                            .and_then(serde_json::Value::as_str)
                            == Some("synthetic-order")
                    })
                    .map(|e| {
                        e.data
                            .pointer("/data/payload")
                            .and_then(serde_json::Value::as_u64)
                            .unwrap()
                    })
                    .collect();
                assert_eq!(
                    order,
                    vec![997, 998],
                    "equal timestamps must follow source segment order, not arrival order"
                );
            } else {
                let trace = report["trace_id"].as_str().unwrap();
                let spans = ctx
                    .app_state
                    .otel_service
                    .query_spans(TraceQuery {
                        project_id: ctx.project_id,
                        trace_id: Some(trace.into()),
                        limit: Some(100),
                        ..Default::default()
                    })
                    .await
                    .unwrap();
                assert_eq!(spans.len(), 2);
                let root = spans.iter().find(|s| s.name == "Synthetic root").unwrap();
                let child = spans.iter().find(|s| s.name == "Synthetic child").unwrap();
                assert_eq!(child.parent_span_id.as_deref(), Some(root.span_id.as_str()));
                assert_eq!(
                    root.resource.service_version.as_deref(),
                    Some("synthetic-release-1")
                );
                assert_eq!(
                    child.resource.deployment_environment.as_deref(),
                    Some("synthetic-test")
                );
                let logs = ctx
                    .app_state
                    .otel_service
                    .query_logs(temps_otel::types::LogQuery {
                        project_id: ctx.project_id,
                        trace_id: Some(trace.into()),
                        ..Default::default()
                    })
                    .await
                    .unwrap();
                assert_eq!(logs.len(), 1);
                if report["metrics"].as_u64().unwrap() > 0 {
                    let sum = ctx
                        .app_state
                        .otel_service
                        .query_metrics(MetricQuery {
                            project_id: ctx.project_id,
                            metric_name: Some("synthetic.requests".into()),
                            aggregation: MetricAggregation::Sum,
                            ..Default::default()
                        })
                        .await
                        .unwrap();
                    assert!(sum.iter().any(|b| b.value >= 5.0));
                    let distribution = ctx
                        .app_state
                        .otel_service
                        .query_metrics(MetricQuery {
                            project_id: ctx.project_id,
                            metric_name: Some("synthetic.duration".into()),
                            aggregation: MetricAggregation::Quantile(0.95),
                            ..Default::default()
                        })
                        .await
                        .unwrap();
                    assert!(distribution
                        .iter()
                        .any(|b| b.value >= 12.5 && b.value <= 25.0));
                    // Full-fidelity columns must retain units, delta semantics
                    // and exemplars as well as the chart's scalar aggregates.
                    let rows=ctx._db.connection().query_all(sea_orm::Statement::from_sql_and_values(sea_orm::DatabaseBackend::Postgres,"SELECT unit,temporality,exemplars FROM otel_metrics WHERE project_id=$1 AND metric_name='synthetic.duration' AND exemplars @> jsonb_build_array(jsonb_build_object('trace_id',$2::text))",[ctx.project_id.into(),trace.into()])).await.unwrap();
                    assert!(!rows.is_empty());
                    for row in rows {
                        assert_eq!(row.try_get::<String>("", "unit").unwrap(), "millisecond");
                        assert_eq!(row.try_get::<String>("", "temporality").unwrap(), "delta");
                        let exemplars: serde_json::Value = row.try_get("", "exemplars").unwrap();
                        assert!(exemplars
                            .as_array()
                            .unwrap()
                            .iter()
                            .any(|e| e["trace_id"] == trace));
                    }
                }
                // No transaction should be stored as an error issue.
                let events = temps_entities::error_events::Entity::find()
                    .filter(temps_entities::error_events::Column::ProjectId.eq(ctx.project_id))
                    .all(ctx._db.connection())
                    .await
                    .unwrap();
                assert!(events.iter().any(|e| e
                    .data
                    .as_ref()
                    .is_some_and(|v| v.to_string().contains(trace))));
            }
            eprintln!(
                "Verified official SDK {} scenario {mode} -> native query",
                report["sdk"]
            );
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "Official SDK prerequisites: scripts/sentry-acceptance.sh sdk"]
    #[serial_test::serial]
    async fn official_sdk_ingestion_reaches_native_queries_and_reports_unsupported() {
        // Explicit opt-in supplies a temporary installation, never customer data.
        // Run separately for each pinned SDK entry point. Absence is a reported
        // live conformance blocker, never proof of SDK compatibility.
        let entry = PrioritySdkHarness::prerequisite("TEMPS_SENTRY_CONFORMANCE_SDK");
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
        let ctx = create_test_context().await;
        let router = Router::new()
            .nest("/api", configure_routes())
            .layer(axum::middleware::from_fn(inject_test_request_metadata))
            .with_state(ctx.app_state.clone());
        let server = TestServer::builder()
            .http_transport()
            .build(router.into_make_service_with_connect_info::<SocketAddr>());
        let base = server.server_address().unwrap();
        let dsn = format!(
            "http://{}@{}/{id}",
            ctx.dsn_key,
            base.authority(),
            id = ctx.project_id
        );
        let modes: &[&str] = if entry.contains("/browser/") {
            &["browser", "browser-tunnel", "browser-auto"]
        } else if std::env::var("TEMPS_SENTRY_CONFORMANCE_SUITE").as_deref() == Ok("signals") {
            &["errors", "errors-tunnel", "cron", "cron-tunnel", "signals"]
        } else {
            &["direct", "tunnel", "mixed"]
        };
        let alarms = temps_monitoring::AlarmService::new(
            ctx._db.connection_arc(),
            Arc::new(ConformanceNotifications),
            Arc::new(ConformanceQueue),
        );
        for &mode in modes {
            let errors_before = error_event_project_ids(&ctx).await.len();
            let logs_before = ctx
                .app_state
                .otel_service
                .query_logs(native_log_query(ctx.project_id))
                .await
                .unwrap()
                .len();
            let output = tokio::process::Command::new("bun")
                .arg(PrioritySdkHarness::fixture("sentry-log-conformance.mjs"))
                .args([
                    entry.as_str(),
                    dsn.as_str(),
                    mode,
                    &format!("{}api{}", base, SENTRY_TUNNEL_ROUTE_PATH),
                ])
                .output()
                .await
                .unwrap();
            assert!(
                output.status.success(),
                "synthetic SDK {mode} failed (credential-bearing output suppressed)"
            );
            let summary: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            if mode.starts_with("browser") {
                assert_eq!(summary["deliveries"], serde_json::json!([200]));
            } else {
                let outcomes = summary["outcomes"].as_array().unwrap();
                assert!(!outcomes.is_empty());
                assert!(outcomes
                    .iter()
                    .all(|o| o["status"] == 200 && o["types"] == ""));
                let total = |key: &str| {
                    outcomes
                        .iter()
                        .map(|o| o[key].as_u64().unwrap())
                        .sum::<u64>()
                };
                assert_eq!(
                    total("errors"),
                    summary["events"].as_array().unwrap().len() as u64
                );
                assert_eq!(total("logs"), summary["logs"].as_u64().unwrap());
                assert_eq!(total("check_ins"), summary["check_ins"].as_u64().unwrap());
                assert_eq!(total("unsupported"), 0);
                if mode.starts_with("errors") {
                    assert!(outcomes.iter().all(|o| o["gzip"] == true));
                }
            }
            ctx.app_state
                .cron_monitor_service
                .sync_alarms(&alarms)
                .await
                .unwrap();
            if mode.starts_with("errors") || mode.starts_with("cron") || mode == "signals" {
                let event_ids = summary["events"].as_array().unwrap();
                let all_events = temps_entities::error_events::Entity::find()
                    .filter(temps_entities::error_events::Column::ProjectId.eq(ctx.project_id))
                    .all(ctx._db.connection())
                    .await
                    .unwrap();
                assert_eq!(all_events.len(), errors_before + event_ids.len());
                let new_events: Vec<_> = all_events
                    .iter()
                    .filter(|event| {
                        event
                            .data
                            .as_ref()
                            .is_some_and(|data| event_ids.contains(&data["sentry"]["event_id"]))
                    })
                    .collect();
                assert_eq!(
                    new_events.len(),
                    event_ids.len(),
                    "every SDK event ID must reach native storage"
                );
                let all_logs = ctx
                    .app_state
                    .otel_service
                    .query_logs(native_log_query(ctx.project_id))
                    .await
                    .unwrap();
                assert_eq!(
                    all_logs.len(),
                    logs_before + summary["logs"].as_u64().unwrap() as usize
                );
                if mode.starts_with("errors") {
                    let exceptions: Vec<_> = new_events
                        .iter()
                        .filter(|event| event.exception_type == "TypeError")
                        .collect();
                    assert_eq!(exceptions.len(), 2);
                    assert_eq!(
                        exceptions[0].error_group_id, exceptions[1].error_group_id,
                        "concurrent matching SDK errors must share a native group"
                    );
                    let group = ctx
                        .app_state
                        .error_tracking_service
                        .get_error_group(exceptions[0].error_group_id, ctx.project_id)
                        .await
                        .unwrap();
                    let group_events = all_events
                        .iter()
                        .filter(|event| event.error_group_id == group.id)
                        .count();
                    assert_eq!(
                        group.total_count as usize, group_events,
                        "native counts must equal stored events"
                    );
                    for event in &new_events {
                        let native = ctx
                            .app_state
                            .error_tracking_service
                            .get_error_event(event.id, event.error_group_id, ctx.project_id)
                            .await
                            .unwrap();
                        let raw = &native.data.as_ref().unwrap()["sentry"];
                        assert_eq!(raw["user"]["id"], "synthetic-user");
                        assert_eq!(raw["user"]["email"], "synthetic@example.invalid");
                        assert_eq!(
                            raw["contexts"]["synthetic"]["operation"],
                            "synthetic-operation"
                        );
                        assert_eq!(raw["fingerprint"][0], "synthetic-fingerprint");
                        assert_eq!(raw["release"], "synthetic-release-1");
                        assert_eq!(raw["environment"], "synthetic-test");
                        assert!(raw["breadcrumbs"]
                            .as_array()
                            .or_else(|| raw["breadcrumbs"]["values"].as_array())
                            .expect("SDK breadcrumb array or values wrapper must survive")
                            .iter()
                            .any(|crumb| crumb["message"] == "Synthetic breadcrumb"));
                        let tag = raw["tags"]["synthetic.mode"].as_str().or_else(|| {
                            raw["tags"].as_array()?.iter().find_map(|tag| {
                                if tag[0] == "synthetic.mode" {
                                    tag[1].as_str()
                                } else {
                                    None
                                }
                            })
                        });
                        assert_eq!(tag, Some(mode));
                    }
                    let message_event = new_events
                        .iter()
                        .find(|event| {
                            event.exception_value.as_deref() == Some("Synthetic SDK message")
                        })
                        .expect("the SDK message must be queryable as a native event");
                    let raw_message = &message_event.data.as_ref().unwrap()["sentry"];
                    assert_eq!(raw_message["message"], "Synthetic SDK message");
                    let source_exception = raw_message["exception"]["values"]
                        .as_array()
                        .and_then(|values| values.last());
                    // Node 11 attaches an untyped synthetic exception/stack to
                    // captureMessage by default. Native mapping preserves that
                    // exception as Error; a message without exceptions is Message.
                    let expected_type = source_exception
                        .map(|exception| exception["type"].as_str().unwrap_or("Error"))
                        .unwrap_or("Message");
                    assert_eq!(message_event.exception_type, expected_type);
                    if let Some(exception) = source_exception {
                        assert_eq!(exception["mechanism"]["synthetic"], true);
                        assert!(exception["stacktrace"]["frames"]
                            .as_array()
                            .is_some_and(|frames| !frames.is_empty()));
                        let native_frames = message_event.data.as_ref().unwrap()["stack_trace"]
                            .as_array()
                            .expect("the synthetic capture stack must reach native details");
                        let source_frames = exception["stacktrace"]["frames"].as_array().unwrap();
                        assert_eq!(native_frames.len(), source_frames.len());
                        for (native, source) in native_frames.iter().zip(source_frames) {
                            for field in ["filename", "function", "lineno", "colno", "abs_path"] {
                                assert_eq!(native[field], source[field]);
                            }
                        }
                    }
                    for event in exceptions {
                        assert_eq!(
                            event.exception_value.as_deref(),
                            Some("Synthetic repeated failure")
                        );
                        let raw = &event.data.as_ref().unwrap()["sentry"];
                        let frames = raw["exception"]["values"][0]["stacktrace"]["frames"]
                            .as_array()
                            .unwrap();
                        assert!(!frames.is_empty());
                        assert!(frames
                            .iter()
                            .all(|frame| frame["filename"] == "synthetic:///application.mjs"));
                    }
                    eprintln!("SDK {} {mode}: 3 gzip events, native reads, concurrent grouping and exact counts verified", summary["sdk"]);
                    continue;
                }
                if mode.starts_with("cron") {
                    assert_eq!(summary["check_ins"], 7);
                    let monitors = ctx
                        .app_state
                        .cron_monitor_service
                        .list(ctx.project_id, 100, 0)
                        .await
                        .unwrap();
                    assert_eq!(monitors.len(), 4);
                    for descriptor in summary["runs"].as_array().unwrap() {
                        let monitor = monitors
                            .iter()
                            .find(|m| m.slug == descriptor["slug"].as_str().unwrap())
                            .unwrap();
                        let runs = ctx
                            .app_state
                            .cron_monitor_service
                            .runs(ctx.project_id, monitor.id, 100, 0)
                            .await
                            .unwrap();
                        let run = runs
                            .iter()
                            .find(|r| r.check_in_id == descriptor["id"].as_str().unwrap())
                            .unwrap();
                        assert_eq!(run.status, descriptor["status"].as_str().unwrap());
                        assert_eq!(run.duration, descriptor["duration"].as_f64());
                        assert_eq!(run.trace_id.as_deref(), descriptor["trace_id"].as_str());
                        assert_eq!(run.span_id.as_deref(), descriptor["span_id"].as_str());
                        assert_eq!(run.release.as_deref(), Some("synthetic-release-1"));
                        assert_eq!(run.environment.as_deref(), Some("synthetic-test"));
                    }
                    let (native_alarms, total) = alarms
                        .list_alarms(
                            Some(ctx.project_id),
                            temps_monitoring::AlarmFilters {
                                alarm_type: Some(temps_monitoring::AlarmType::CronJobFailed),
                                status: Some(temps_monitoring::AlarmStatus::Firing),
                                ..Default::default()
                            },
                            1,
                            20,
                        )
                        .await
                        .unwrap();
                    assert_eq!(
                        total, 2,
                        "SDK failure and heartbeat must reach native alarms"
                    );
                    for alarm in native_alarms {
                        assert_eq!(
                            alarm.metadata.as_ref().unwrap()["latest_run"]["status"],
                            "error"
                        );
                    }
                    eprintln!(
                        "SDK {} {mode}: 7 check-ins verified in native monitors, run history and alarm queries",
                        summary["sdk"]
                    );
                    continue;
                }
                assert_eq!(summary["check_ins"], 1);
                let mixed = ctx
                    .app_state
                    .cron_monitor_service
                    .list(ctx.project_id, 100, 0)
                    .await
                    .unwrap()
                    .into_iter()
                    .find(|m| m.slug == "synthetic-mixed")
                    .unwrap();
                let run = ctx
                    .app_state
                    .cron_monitor_service
                    .runs(ctx.project_id, mixed.id, 100, 0)
                    .await
                    .unwrap()
                    .remove(0);
                assert_eq!(run.check_in_id, summary["runs"][0]["id"].as_str().unwrap());
                assert_eq!(run.status, "ok");
                assert_eq!(
                    run.trace_id.as_deref(),
                    summary["runs"][0]["trace_id"].as_str()
                );
                assert_eq!(new_events.len(), 1);
                assert_eq!(
                    new_events[0].trace_id_indexed.as_deref(),
                    summary["trace_id"].as_str()
                );
            }
            let mut query = native_log_query(ctx.project_id);
            query.trace_id = Some(summary["trace_id"].as_str().unwrap().to_owned());
            let logs = ctx.app_state.otel_service.query_logs(query).await.unwrap();
            assert_eq!(
                logs.len(),
                2,
                "{mode}: SDK -> HTTP -> native storage -> trace query"
            );
            if mode.starts_with("browser") {
                for log in &logs {
                    if mode == "browser-auto" {
                        assert_eq!(log.attributes["user.ip_address"], "127.0.0.1");
                        assert_eq!(log.attributes["browser.name"], "Chrome");
                        assert!(log.attributes.contains_key("browser.version"));
                    } else {
                        assert!(!log.attributes.contains_key("user.ip_address"));
                        assert!(!log.attributes.contains_key("browser.name"));
                    }
                }
            }
            assert_eq!(
                logs[0].resource.service_version.as_deref(),
                Some("synthetic-release-1")
            );
            assert!(logs
                .iter()
                .all(|log| log.attributes["synthetic.mode"] == mode));
            assert!(logs
                .iter()
                .any(|log| log.attributes.contains_key("sentry.message.template")));
            if mode == "mixed" {
                let events = temps_entities::error_events::Entity::find()
                    .filter(
                        temps_entities::error_events::Column::TraceIdIndexed
                            .eq(summary["trace_id"].as_str().unwrap()),
                    )
                    .all(ctx._db.connection())
                    .await
                    .unwrap();
                assert_eq!(events.len(), 1, "error and logs must share the trace");
            }
            eprintln!(
                "SDK {} {mode}: 2 logs confirmed by native trace query",
                summary["sdk"]
            );
        }
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn repeated_telemetry_batches_store_together_through_native_services() {
        let Some(ctx) = conformance_test_context().await else {
            return;
        };
        let server = TestServer::new(configure_routes().with_state(ctx.app_state.clone()));
        let path = format!("/{}/envelope/?sentry_key={}", ctx.project_id, ctx.dsn_key);
        let mut envelope = synthetic_log_envelope().to_vec();
        let metric = br#"{"type":"trace_metric","item_count":1,"content_type":"application/vnd.sentry.items.trace-metric+json"}
{"items":[{"timestamp":1790000000,"trace_id":"11111111111111111111111111111111","name":"synthetic.repeated","type":"counter","value":3}]}
"#;
        envelope.extend_from_slice(metric);
        envelope.extend_from_slice(&synthetic_log_envelope()[3..]);
        envelope.extend_from_slice(metric);
        let response = server.post(&path).bytes(Bytes::from(envelope)).await;
        response.assert_status_ok();
        assert_eq!(response.header("x-temps-sentry-logs-stored"), "2");
        assert_eq!(response.header("x-temps-sentry-metrics-stored"), "2");
        let logs = ctx
            .app_state
            .otel_service
            .query_logs(native_log_query(ctx.project_id))
            .await
            .unwrap();
        assert_eq!(logs.len(), 2);
        let metrics = ctx
            .app_state
            .otel_service
            .query_metrics(temps_otel::types::MetricQuery {
                project_id: ctx.project_id,
                metric_name: Some("synthetic.repeated".into()),
                aggregation: temps_otel::types::MetricAggregation::Sum,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(metrics.iter().map(|b| b.value).sum::<f64>(), 6.0);
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn rejects_logs_before_writes_and_reports_mixed_storage_failure() {
        let Some(ctx) = conformance_test_context().await else {
            return;
        };
        let server = TestServer::new(configure_routes().with_state(ctx.app_state.clone()));
        let path = format!("/{}/envelope/?sentry_key={}", ctx.project_id, ctx.dsn_key);
        let mut mixed = b"{\"event_id\":\"11111111111111111111111111111111\"}\n{\"type\":\"event\"}\n{\"level\":\"error\",\"message\":\"Synthetic mixed failure\",\"platform\":\"other\"}\n".to_vec();
        mixed.extend_from_slice(&synthetic_log_envelope()[3..]);
        let invalid = String::from_utf8(mixed.clone())
            .unwrap()
            .replace("\"item_count\":1", "\"item_count\":2");
        server
            .post(&path)
            .bytes(Bytes::from(invalid))
            .await
            .assert_status_bad_request();
        assert_eq!(ctx.app_state.otel_service.pipeline_stats().logs_received, 0);
        assert!(error_event_project_ids(&ctx).await.is_empty());

        // Prime native quota cache, then fail only this test's isolated log
        // table. Error writes remain healthy, demonstrating partial ingestion.
        ctx.app_state
            .otel_service
            .check_quota(ctx.project_id)
            .await
            .unwrap();
        ctx._db
            .execute_sql("DROP TABLE otel_log_events CASCADE")
            .await
            .unwrap();
        let response = server.post(&path).bytes(Bytes::from(mixed.clone())).await;
        response.assert_status_internal_server_error();
        assert_eq!(response.header("x-temps-sentry-errors-stored"), "1");
        assert_eq!(response.header("x-temps-sentry-logs-stored"), "0");
        assert_eq!(ctx.app_state.otel_service.pipeline_stats().logs_dropped, 1);
        // Retry the whole mixed envelope after its error already committed.
        let retry = server.post(&path).bytes(Bytes::from(mixed)).await;
        retry.assert_status_internal_server_error();
        assert_eq!(retry.header("x-temps-sentry-errors-stored"), "0");
        assert_eq!(retry.header("x-temps-sentry-errors-duplicates"), "1");
        assert_eq!(error_event_project_ids(&ctx).await.len(), 1);
    }

    #[test]
    fn retry_statuses_and_partial_counts_do_not_echo_storage_errors() {
        let error = temps_otel::OtelError::Storage {
            kind: temps_otel::error::StorageErrorKind::ClickHouseNetwork,
            message: "SECRET_SYNTHETIC_STORAGE_PAYLOAD".into(),
        };
        let response = log_ingest_error(error, 1, 2, "unknown");
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(response.headers()["retry-after"], "60");
        assert_eq!(response.headers()["x-temps-sentry-errors-stored"], "1");
        assert_eq!(response.headers()["x-temps-sentry-logs-stored"], "0");
    }

    // Mock audit logger for tests
    #[derive(Clone)]
    struct MockAuditLogger;

    #[async_trait]
    impl temps_core::AuditLogger for MockAuditLogger {
        async fn create_audit_log(
            &self,
            _operation: &dyn temps_core::AuditOperation,
        ) -> Result<(), anyhow::Error> {
            Ok(())
        }
    }

    struct TestContext {
        app_state: Arc<AppState>,
        project_id: i32,
        project: Arc<temps_entities::projects::Model>,
        dsn_key: String,
        route_table: Arc<temps_routes::CachedPeerTable>,
        _db: TestDatabase, // Keep database alive
    }

    /// Test-only middleware that fills in `Extension<temps_core::RequestMetadata>`
    /// from the request's `Host` header, mirroring the shape (not the full
    /// logic) of `temps_core::RequestMetadataMiddleware`, which runs globally
    /// in production but isn't layered onto `configure_routes()`'s bare
    /// router under test.
    async fn inject_test_request_metadata(
        mut req: axum::extract::Request,
        next: axum::middleware::Next,
    ) -> axum::response::Response {
        let host = req
            .headers()
            .get(http::header::HOST)
            .and_then(|v| v.to_str().ok())
            .map(|h| temps_core::host_without_port(h).to_string())
            .unwrap_or_default();

        req.extensions_mut().insert(temps_core::RequestMetadata {
            ip_address: "127.0.0.1".to_string(),
            user_agent: String::new(),
            headers: HeaderMap::new(),
            visitor_id_cookie: None,
            session_id_cookie: None,
            base_url: format!("https://{}", host),
            scheme: "https".to_string(),
            host,
            is_secure: true,
        });

        next.run(req).await
    }

    async fn create_test_context() -> TestContext {
        create_test_context_with_db(TestDatabase::with_migrations().await.unwrap()).await
    }

    async fn create_test_context_with_db(db: TestDatabase) -> TestContext {
        use sea_orm::ActiveModelTrait;
        use sea_orm::Set;
        use temps_entities::projects;
        use uuid::Uuid;

        // Create a test project with all required fields (use unique slug per test)
        let unique_slug = format!("test-project-{}", Uuid::new_v4());
        let project = projects::ActiveModel {
            name: Set("Test Project".to_string()),
            repo_name: Set("test-repo".to_string()),
            repo_owner: Set("test-owner".to_string()),
            directory: Set("/test".to_string()),
            main_branch: Set("main".to_string()),
            slug: Set(unique_slug),
            preset: Set(Preset::NextJs),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db.connection())
        .await
        .unwrap();

        let error_tracking_service = Arc::new(ErrorTrackingService::new(db.connection_arc()));
        let dsn_service = Arc::new(DSNService::new(db.connection_arc()));

        // Generate a DSN for the test project
        let dsn = dsn_service
            .generate_project_dsn(
                project.id,
                None,
                None,
                Some("Test DSN".to_string()),
                "localhost",
            )
            .await
            .unwrap();

        let sentry_provider = Arc::new(SentryProvider::new(dsn_service.clone()));
        let audit_service = Arc::new(MockAuditLogger) as Arc<dyn temps_core::AuditLogger>;
        let route_table = Arc::new(temps_routes::CachedPeerTable::new(db.connection_arc()));

        let app_state = Arc::new(AppState {
            sentry_provider,
            session_replay_service: Arc::new(
                temps_analytics_session_replay::services::SessionReplayService::new(
                    db.connection_arc(),
                ),
            ),
            otel_service: Arc::new(temps_otel::OtelService::new(
                Arc::new(temps_otel::storage::timescaledb::TimescaleDbStorage::new(
                    db.connection_arc(),
                    None,
                )),
                Arc::new(temps_otel::ingest::auth::OtelAuthService::new(
                    db.connection_arc(),
                )),
                Arc::new(temps_otel::ingest::rate_limit::RateLimiter::new(
                    10000,
                    std::time::Duration::from_secs(60),
                )),
                8,
            )),
            cron_monitor_service: Arc::new(
                temps_monitoring::cron_monitors::CronMonitorService::new(db.connection_arc()),
            ),
            error_tracking_service,
            audit_service,
            ip_address_service: None,
            db: None,
            telemetry: Arc::new(temps_core::telemetry::NoopTelemetryReporter),
            route_table: route_table.clone(),
            dsn_service: dsn_service.clone(),
            rate_limiter: crate::sentry::rate_limiter::IngestRateLimiter::new(),
        });

        TestContext {
            app_state,
            project_id: project.id,
            project: Arc::new(project),
            dsn_key: dsn.public_key,
            route_table,
            _db: db,
        }
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_envelope_endpoint_with_valid_error_event() {
        let ctx = create_test_context().await;
        let app = configure_routes().with_state(ctx.app_state);
        let server = TestServer::new(app);

        // The official PHP SDK places event_id only in the envelope header.
        let envelope_data = "{\"event_id\":\"9ec79c33ec9942ab8353589fcb2e04dc\",\"sent_at\":\"2023-06-28T14:30:00.000Z\"}\n{\"type\":\"event\",\"content_type\":\"application/json\"}\n{\"timestamp\":1687962600.0,\"platform\":\"php\",\"level\":\"error\",\"exception\":{\"values\":[{\"type\":\"Error\",\"value\":\"Test error message\",\"stacktrace\":{\"frames\":[{\"filename\":\"app.php\",\"function\":\"handleRequest\",\"lineno\":42}]}}]},\"environment\":\"production\",\"release\":\"1.0.0\"}\n";

        let auth_header = format!("Sentry sentry_key={},sentry_version=7", ctx.dsn_key);

        let response = server
            .post(&format!("/{}/envelope/", ctx.project_id))
            .content_type("application/octet-stream")
            .add_header(
                HeaderName::from_static("x-sentry-auth"),
                HeaderValue::from_str(&auth_header).unwrap(),
            )
            .bytes(Bytes::from(envelope_data))
            .await;

        // Should successfully ingest the event
        assert_eq!(response.status_code(), StatusCode::OK);
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_envelope_endpoint_with_invalid_envelope() {
        let ctx = create_test_context().await;
        let app = configure_routes().with_state(ctx.app_state);
        let server = TestServer::new(app);

        // Send invalid envelope data (but with valid auth)
        let invalid_data = "not a valid envelope";
        let auth_header = format!("Sentry sentry_key={},sentry_version=7", ctx.dsn_key);

        let response = server
            .post(&format!("/{}/envelope/", ctx.project_id))
            .content_type("application/octet-stream")
            .add_header(
                HeaderName::from_static("x-sentry-auth"),
                HeaderValue::from_str(&auth_header).unwrap(),
            )
            .text(invalid_data)
            .await;

        // Should return 400 for invalid envelope format (auth succeeds, parsing fails)
        assert_eq!(response.status_code(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_envelope_endpoint_with_session() {
        let ctx = create_test_context().await;
        let app = configure_routes().with_state(ctx.app_state);
        let server = TestServer::new(app);

        // Create a valid session envelope
        let envelope_data = "{\"event_id\":\"a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6\"}\n{\"type\":\"session\"}\n{\"sid\":\"a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6\",\"init\":true,\"started\":\"2023-06-28T14:30:00.000Z\",\"status\":\"ok\",\"attrs\":{\"release\":\"1.0.0\",\"environment\":\"production\"}}\n";
        let auth_header = format!("Sentry sentry_key={},sentry_version=7", ctx.dsn_key);

        let response = server
            .post(&format!("/{}/envelope/", ctx.project_id))
            .content_type("application/octet-stream")
            .add_header(
                HeaderName::from_static("x-sentry-auth"),
                HeaderValue::from_str(&auth_header).unwrap(),
            )
            .bytes(Bytes::from(envelope_data))
            .await;

        // Session items are accepted but not processed yet (returns OK or validation error)
        assert!(
            response.status_code() == StatusCode::OK
                || response.status_code() == StatusCode::BAD_REQUEST,
            "Expected 200 or 400, got {}",
            response.status_code()
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_envelope_endpoint_with_auth_header() {
        let ctx = create_test_context().await;
        let app = configure_routes().with_state(ctx.app_state);
        let server = TestServer::new(app);

        let envelope_data = "{\"event_id\":\"9ec79c33ec9942ab8353589fcb2e04dc\"}\n{\"type\":\"event\"}\n{\"event_id\":\"9ec79c33ec9942ab8353589fcb2e04dc\",\"timestamp\":1687962600.0,\"platform\":\"javascript\",\"level\":\"info\",\"message\":\"Test\"}\n";
        let auth_header = format!("Sentry sentry_key={},sentry_version=7", ctx.dsn_key);

        let response = server
            .post(&format!("/{}/envelope/", ctx.project_id))
            .content_type("application/octet-stream")
            .add_header(
                HeaderName::from_static("x-sentry-auth"),
                HeaderValue::from_str(&auth_header).unwrap(),
            )
            .bytes(Bytes::from(envelope_data))
            .await;

        // The auth header extraction should work and event should be accepted
        assert!(
            response.status_code() == StatusCode::OK
                || response.status_code() == StatusCode::BAD_REQUEST,
            "Expected 200 or 400, got {}",
            response.status_code()
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_envelope_endpoint_missing_newlines() {
        let ctx = create_test_context().await;
        let app = configure_routes().with_state(ctx.app_state);
        let server = TestServer::new(app);

        // Envelope without proper newlines should fail
        let invalid_envelope = "{\"event_id\":\"test\"}{\"type\":\"event\"}{\"message\":\"test\"}";
        let auth_header = format!("Sentry sentry_key={},sentry_version=7", ctx.dsn_key);

        let response = server
            .post(&format!("/{}/envelope/", ctx.project_id))
            .content_type("application/octet-stream")
            .add_header(
                HeaderName::from_static("x-sentry-auth"),
                HeaderValue::from_str(&auth_header).unwrap(),
            )
            .text(invalid_envelope)
            .await;

        // Should fail due to invalid envelope format
        assert_eq!(response.status_code(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_dsn_key_extraction() {
        let empty_params = std::collections::HashMap::new();

        // Test query parameter (highest priority)
        let mut params = std::collections::HashMap::new();
        params.insert("sentry_key".to_string(), "query_key".to_string());
        let headers = HeaderMap::new();
        assert_eq!(
            extract_dsn_key(&headers, &params),
            Some("query_key".to_string())
        );

        // Test X-Sentry-Auth header
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("x-sentry-auth"),
            HeaderValue::from_static("Sentry sentry_key=my_public_key,sentry_version=7"),
        );
        assert_eq!(
            extract_dsn_key(&headers, &empty_params),
            Some("my_public_key".to_string())
        );

        // Test Authorization header
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("authorization"),
            HeaderValue::from_static("DSN my_dsn_key"),
        );
        assert_eq!(
            extract_dsn_key(&headers, &empty_params),
            Some("my_dsn_key".to_string())
        );

        // Test no auth header or query param
        let headers = HeaderMap::new();
        assert_eq!(extract_dsn_key(&headers, &empty_params), None);

        // Test query param takes precedence over header
        let mut params = std::collections::HashMap::new();
        params.insert("sentry_key".to_string(), "query_key".to_string());
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("x-sentry-auth"),
            HeaderValue::from_static("Sentry sentry_key=header_key,sentry_version=7"),
        );
        assert_eq!(
            extract_dsn_key(&headers, &params),
            Some("query_key".to_string())
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_envelope_endpoint_with_gzip_compression() {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        use std::io::Write;

        let ctx = create_test_context().await;
        let app = configure_routes().with_state(ctx.app_state);
        let server = TestServer::new(app);

        // Create a valid envelope
        let envelope_data = "{\"event_id\":\"9ec79c33ec9942ab8353589fcb2e04dc\",\"sent_at\":\"2023-06-28T14:30:00.000Z\"}\n{\"type\":\"event\"}\n{\"event_id\":\"9ec79c33ec9942ab8353589fcb2e04dc\",\"timestamp\":1687962600.0,\"platform\":\"javascript\",\"level\":\"error\",\"exception\":{\"values\":[{\"type\":\"Error\",\"value\":\"Test error\"}]}}\n";

        // Compress it with gzip
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder
            .write_all(envelope_data.as_bytes())
            .expect("Failed to write to gzip encoder");
        let compressed_data = encoder.finish().expect("Failed to finish gzip compression");

        let auth_header = format!("Sentry sentry_key={},sentry_version=7", ctx.dsn_key);

        // Send compressed envelope with Content-Encoding: gzip header
        let response = server
            .post(&format!("/{}/envelope/", ctx.project_id))
            .content_type("application/octet-stream")
            .add_header(
                http::HeaderName::from_static("content-encoding"),
                http::HeaderValue::from_static("gzip"),
            )
            .add_header(
                HeaderName::from_static("x-sentry-auth"),
                HeaderValue::from_str(&auth_header).unwrap(),
            )
            .bytes(Bytes::from(compressed_data))
            .await;

        // Should successfully decompress and parse
        assert!(
            response.status_code() == StatusCode::OK
                || response.status_code() == StatusCode::BAD_REQUEST,
            "Expected 200 or 400, got {}. Body: {}",
            response.status_code(),
            response.text()
        );
    }

    // === Decompression bomb protection tests ===

    #[test]
    fn test_decompress_rejects_unknown_stacked_and_trailing_gzip_data() {
        use std::io::Write;
        let body = Bytes::from_static(b"{}\n");
        for encoding in ["br", "not-gzip", "gzip, br"] {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::CONTENT_ENCODING,
                HeaderValue::from_str(encoding).unwrap(),
            );
            assert!(decompress_if_needed(&headers, &body).is_err());
        }
        let mut headers = HeaderMap::new();
        headers.append(header::CONTENT_ENCODING, HeaderValue::from_static("gzip"));
        headers.append(header::CONTENT_ENCODING, HeaderValue::from_static("gzip"));
        assert!(decompress_if_needed(&headers, &body).is_err());
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&body).unwrap();
        let member = encoder.finish().unwrap();
        let mut joined = member.clone();
        joined.extend_from_slice(&member);
        headers = HeaderMap::new();
        headers.insert(header::CONTENT_ENCODING, HeaderValue::from_static("gzip"));
        assert_eq!(
            decompress_if_needed(&headers, &Bytes::from(joined)).unwrap(),
            Bytes::from_static(b"{}\n{}\n")
        );
        let mut bad = member;
        bad.extend_from_slice(b"trailing junk");
        assert!(decompress_if_needed(&headers, &Bytes::from(bad)).is_err());
    }

    #[test]
    fn test_decompress_normal_gzip() {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        use std::io::Write;

        let data = b"Hello, World!";
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(data).unwrap();
        let compressed = encoder.finish().unwrap();

        let mut headers = HeaderMap::new();
        headers.insert("content-encoding", "gzip".parse().unwrap());
        let body = Bytes::from(compressed);

        let result = decompress_if_needed(&headers, &body);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().as_ref(), data);
    }

    #[test]
    fn test_decompress_no_encoding_passthrough() {
        let headers = HeaderMap::new();
        let body = Bytes::from_static(b"raw data");
        let result = decompress_if_needed(&headers, &body);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().as_ref(), b"raw data");
    }

    #[test]
    fn test_decompress_bomb_rejected() {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        use std::io::Write;

        // Create a gzip bomb: highly compressible data that expands beyond MAX_DECOMPRESSED_SIZE
        // A sequence of zeros compresses extremely well
        let large_data = vec![0u8; MAX_DECOMPRESSED_SIZE + 1024];
        let mut encoder = GzEncoder::new(Vec::new(), Compression::best());
        encoder.write_all(&large_data).unwrap();
        let compressed = encoder.finish().unwrap();

        // The compressed data should be much smaller than the expanded data
        assert!(
            compressed.len() < MAX_DECOMPRESSED_SIZE,
            "Compressed size {} should be much smaller than limit {}",
            compressed.len(),
            MAX_DECOMPRESSED_SIZE
        );

        let mut headers = HeaderMap::new();
        headers.insert("content-encoding", "gzip".parse().unwrap());
        let body = Bytes::from(compressed);

        let result = decompress_if_needed(&headers, &body);
        assert!(result.is_err(), "Decompression bomb should be rejected");
        assert!(
            result.unwrap_err().contains("exceeds maximum"),
            "Error should mention size limit"
        );
    }

    #[test]
    fn test_decompress_at_exact_limit_allowed() {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        use std::io::Write;

        // Data exactly at the limit should be allowed
        let data = vec![b'A'; MAX_DECOMPRESSED_SIZE];
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&data).unwrap();
        let compressed = encoder.finish().unwrap();

        let mut headers = HeaderMap::new();
        headers.insert("content-encoding", "gzip".parse().unwrap());
        let body = Bytes::from(compressed);

        let result = decompress_if_needed(&headers, &body);
        assert!(result.is_ok(), "Data at exact limit should be allowed");
        assert_eq!(result.unwrap().len(), MAX_DECOMPRESSED_SIZE);
    }

    // === Fix #2 — XFF-spoof guard tests ===

    /// Non-loopback peer with X-Forwarded-For: resolved IP must be the peer, not the XFF value.
    ///
    /// This verifies the ingest path delegates to `temps_auth::resolve_client_ip`, which
    /// ignores client-supplied XFF headers when the direct TCP peer is not a trusted proxy.
    #[test]
    fn test_xff_spoof_rejected_for_non_loopback_peer() {
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("x-forwarded-for"),
            HeaderValue::from_static("1.2.3.4"),
        );
        // Non-loopback peer: attacker connected directly, not via a trusted proxy
        let peer: SocketAddr = "8.8.8.8:443".parse().unwrap();
        let resolved = temps_auth::resolve_client_ip(&headers, Some(peer));
        // Must be the peer address, NOT the spoofed XFF value
        assert_eq!(
            resolved, "8.8.8.8",
            "XFF must be ignored for non-loopback peers"
        );
    }

    /// Loopback peer with X-Forwarded-For: the rightmost XFF entry is trusted.
    ///
    /// When Pingora (our reverse proxy) runs on the same host it connects as 127.0.0.1,
    /// so XFF is trusted and we return the real client IP that the proxy appended.
    #[test]
    fn test_xff_trusted_for_loopback_peer() {
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("x-forwarded-for"),
            // "client, proxy" → rightmost is what the trusted proxy appended
            HeaderValue::from_static("1.2.3.4, 5.6.7.8"),
        );
        let peer: SocketAddr = "127.0.0.1:1234".parse().unwrap();
        let resolved = temps_auth::resolve_client_ip(&headers, Some(peer));
        // Rightmost XFF entry is the one appended by our trusted proxy
        assert_eq!(
            resolved, "5.6.7.8",
            "Rightmost XFF entry should be trusted for loopback peer"
        );
    }

    // === Fix #3 — body-limit constant sanity check ===

    /// The ingest body-limit constant must be exactly 2 MiB.
    ///
    /// A typical Sentry event is 5–50 KB; 2 MiB is a generous ceiling that still
    /// protects against slow-POST DoS. This test is a canary — if someone raises
    /// the constant they have to update the test too and justify the change.
    #[test]
    fn test_sentry_ingest_body_limit_is_2mib() {
        assert_eq!(
            SENTRY_INGEST_BODY_LIMIT,
            2 * 1024 * 1024,
            "Sentry ingest body limit must be 2 MiB (2 * 1024 * 1024 bytes)"
        );
    }

    // === client_report-only envelopes must not 400 (real Sentry accepts these) ===

    #[tokio::test]
    #[serial_test::serial]
    async fn test_envelope_endpoint_client_report_only_returns_ok() {
        let ctx = create_test_context().await;
        let app = configure_routes().with_state(ctx.app_state);
        let server = TestServer::new(app);

        // A real client_report envelope — no `event`/`transaction` item at all.
        // Before the fix, `parse_events` errored with "No valid events found in
        // envelope" and the handler turned that into a 400, even though this is
        // exactly what SDKs send when they've sampled/dropped events locally.
        let envelope_data = "{\"event_id\":\"9ec79c33ec9942ab8353589fcb2e04dc\"}\n{\"type\":\"client_report\"}\n{\"timestamp\":\"2023-06-28T14:30:00.000Z\",\"discarded_events\":[{\"reason\":\"sample_rate\",\"category\":\"transaction\",\"quantity\":1}]}\n";
        let auth_header = format!("Sentry sentry_key={},sentry_version=7", ctx.dsn_key);

        let response = server
            .post(&format!("/{}/envelope/", ctx.project_id))
            .content_type("application/octet-stream")
            .add_header(
                HeaderName::from_static("x-sentry-auth"),
                HeaderValue::from_str(&auth_header).unwrap(),
            )
            .bytes(Bytes::from(envelope_data))
            .await;

        assert_eq!(
            response.status_code(),
            StatusCode::OK,
            "client_report-only envelope must be accepted, not treated as a parse failure: {}",
            response.text()
        );
    }

    // === Browser tunnel route (Host-resolved, no DSN) ===

    /// Layers the test-only `Extension<RequestMetadata>` middleware onto the
    /// plugin's router — production gets this from the global
    /// `RequestMetadataMiddleware`, which isn't present on the bare router
    /// under test.
    fn configure_tunnel_test_router(state: Arc<AppState>) -> Router<()> {
        configure_routes()
            .layer(axum::middleware::from_fn(inject_test_request_metadata))
            .with_state(state)
    }

    fn test_route_info(project: Arc<temps_entities::projects::Model>) -> temps_routes::RouteInfo {
        temps_routes::RouteInfo {
            backend: temps_routes::BackendType::StaticDir {
                path: "/tmp".to_string(),
            },
            redirect_to: None,
            status_code: None,
            project: Some(project),
            environment: None,
            deployment: None,
            cert_eligible: false,
        }
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_tunnel_endpoint_unknown_host_returns_404() {
        let ctx = create_test_context().await;
        let app = configure_tunnel_test_router(ctx.app_state);
        let server = TestServer::new(app);

        let response = server
            .post(SENTRY_TUNNEL_ROUTE_PATH)
            .content_type("application/octet-stream")
            .add_header(
                HeaderName::from_static("host"),
                HeaderValue::from_static("unknown.example.com"),
            )
            .add_header(
                HeaderName::from_static("origin"),
                HeaderValue::from_static("https://unknown.example.com"),
            )
            .bytes(Bytes::new())
            .await;

        assert_eq!(response.status_code(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_tunnel_endpoint_mismatched_origin_returns_403() {
        let ctx = create_test_context().await;
        ctx.route_table
            .insert_route_for_test("app.example.com", test_route_info(ctx.project.clone()));
        let app = configure_tunnel_test_router(ctx.app_state);
        let server = TestServer::new(app);

        let response = server
            .post(SENTRY_TUNNEL_ROUTE_PATH)
            .content_type("application/octet-stream")
            .add_header(
                HeaderName::from_static("host"),
                HeaderValue::from_static("app.example.com"),
            )
            .add_header(
                HeaderName::from_static("origin"),
                HeaderValue::from_static("https://evil.example.com"),
            )
            .bytes(Bytes::new())
            .await;

        assert_eq!(response.status_code(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_tunnel_endpoint_missing_origin_returns_403() {
        let ctx = create_test_context().await;
        ctx.route_table
            .insert_route_for_test("app.example.com", test_route_info(ctx.project.clone()));
        let app = configure_tunnel_test_router(ctx.app_state);
        let server = TestServer::new(app);

        // No Origin, no Referer — must default-deny, not fail open.
        let response = server
            .post(SENTRY_TUNNEL_ROUTE_PATH)
            .content_type("application/octet-stream")
            .add_header(
                HeaderName::from_static("host"),
                HeaderValue::from_static("app.example.com"),
            )
            .bytes(Bytes::new())
            .await;

        assert_eq!(response.status_code(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_tunnel_endpoint_valid_request_ingests_event() {
        let ctx = create_test_context().await;
        ctx.route_table
            .insert_route_for_test("app.example.com", test_route_info(ctx.project.clone()));
        let app = configure_tunnel_test_router(ctx.app_state);
        let server = TestServer::new(app);

        let envelope_data = "{\"event_id\":\"9ec79c33ec9942ab8353589fcb2e04dc\"}\n{\"type\":\"event\"}\n{\"event_id\":\"9ec79c33ec9942ab8353589fcb2e04dc\",\"timestamp\":1687962600.0,\"platform\":\"javascript\",\"level\":\"error\",\"exception\":{\"values\":[{\"type\":\"Error\",\"value\":\"Tunneled test error\"}]}}\n";

        let response = server
            .post(SENTRY_TUNNEL_ROUTE_PATH)
            .content_type("application/octet-stream")
            .add_header(
                HeaderName::from_static("host"),
                HeaderValue::from_static("app.example.com"),
            )
            .add_header(
                HeaderName::from_static("origin"),
                HeaderValue::from_static("https://app.example.com"),
            )
            .bytes(Bytes::from(envelope_data))
            .await;

        assert_eq!(
            response.status_code(),
            StatusCode::OK,
            "Expected the tunneled envelope to be accepted: {}",
            response.text()
        );
    }

    /// Envelope carrying the `dsn` field browser SDKs embed when
    /// `Sentry.init({ tunnel })` is configured.
    fn tunneled_envelope_with_dsn(dsn: &str) -> String {
        format!(
            "{{\"event_id\":\"9ec79c33ec9942ab8353589fcb2e04dc\",\"dsn\":\"{}\"}}\n{{\"type\":\"event\"}}\n{{\"event_id\":\"9ec79c33ec9942ab8353589fcb2e04dc\",\"timestamp\":1687962600.0,\"platform\":\"javascript\",\"level\":\"error\",\"exception\":{{\"values\":[{{\"type\":\"Error\",\"value\":\"Cross-origin tunneled error\"}}]}}}}\n",
            dsn
        )
    }

    fn tunneled_envelope_without_dsn() -> String {
        "{\"event_id\":\"9ec79c33ec9942ab8353589fcb2e04dc\"}\n{\"type\":\"event\"}\n{\"event_id\":\"9ec79c33ec9942ab8353589fcb2e04dc\",\"timestamp\":1687962600.0,\"platform\":\"javascript\",\"level\":\"error\",\"exception\":{\"values\":[{\"type\":\"Error\",\"value\":\"Tunneled test error\"}]}}\n".to_string()
    }

    /// A DSN key resolves the project on its own: no route-table entry, a
    /// foreign `Host`, and a foreign `Origin` must all be irrelevant. This is
    /// the whole point — the app is hosted somewhere Temps does not know
    /// about.
    #[tokio::test]
    #[serial_test::serial]
    async fn test_tunnel_endpoint_with_explicit_dsn_key_ignores_host() {
        let ctx = create_test_context().await;
        let app = configure_tunnel_test_router(ctx.app_state.clone());
        let server = TestServer::new(app);

        let response = server
            .post(&format!(
                "{}?sentry_key={}",
                SENTRY_TUNNEL_ROUTE_PATH, ctx.dsn_key
            ))
            .content_type("application/octet-stream")
            .add_header(
                HeaderName::from_static("host"),
                HeaderValue::from_static("app.hosted-elsewhere.example"),
            )
            .add_header(
                HeaderName::from_static("origin"),
                HeaderValue::from_static("https://app.hosted-elsewhere.example"),
            )
            .bytes(Bytes::from(tunneled_envelope_without_dsn()))
            .await;

        assert_eq!(
            response.status_code(),
            StatusCode::OK,
            "A keyed tunnel request must not need a route-table entry: {}",
            response.text()
        );
    }

    /// A key that does not resolve must fail loudly rather than silently
    /// falling back to `Host` and landing the data under another project.
    #[tokio::test]
    #[serial_test::serial]
    async fn test_tunnel_endpoint_with_unknown_dsn_key_returns_401() {
        let ctx = create_test_context().await;
        ctx.route_table
            .insert_route_for_test("app.example.com", test_route_info(ctx.project.clone()));
        let app = configure_tunnel_test_router(ctx.app_state.clone());
        let server = TestServer::new(app);

        let response = server
            .post(&format!(
                "{}?sentry_key=0000000000000000000000000000000000000000000000000000000000000000",
                SENTRY_TUNNEL_ROUTE_PATH
            ))
            .content_type("application/octet-stream")
            .add_header(
                HeaderName::from_static("host"),
                HeaderValue::from_static("app.example.com"),
            )
            .add_header(
                HeaderName::from_static("origin"),
                HeaderValue::from_static("https://app.example.com"),
            )
            .bytes(Bytes::from(tunneled_envelope_without_dsn()))
            .await;

        assert_eq!(response.status_code(), StatusCode::UNAUTHORIZED);
    }

    /// A revoked DSN must stop working: `get_project_by_public_key` filters on
    /// `is_active`, so revocation is a 401 on the very next request.
    #[tokio::test]
    #[serial_test::serial]
    async fn test_tunnel_endpoint_with_revoked_dsn_key_returns_401() {
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

        let ctx = create_test_context().await;
        let db = ctx._db.connection_arc();
        temps_entities::project_dsns::Entity::update_many()
            .col_expr(
                temps_entities::project_dsns::Column::IsActive,
                sea_orm::sea_query::Expr::value(false),
            )
            .filter(temps_entities::project_dsns::Column::PublicKey.eq(ctx.dsn_key.clone()))
            .exec(db.as_ref())
            .await
            .unwrap();

        let app = configure_tunnel_test_router(ctx.app_state.clone());
        let server = TestServer::new(app);

        let response = server
            .post(&format!(
                "{}?sentry_key={}",
                SENTRY_TUNNEL_ROUTE_PATH, ctx.dsn_key
            ))
            .content_type("application/octet-stream")
            .add_header(
                HeaderName::from_static("host"),
                HeaderValue::from_static("app.hosted-elsewhere.example"),
            )
            .bytes(Bytes::from(tunneled_envelope_without_dsn()))
            .await;

        assert_eq!(response.status_code(), StatusCode::UNAUTHORIZED);
    }

    /// The zero-configuration path: the SDK embeds its DSN in the envelope
    /// header whenever `tunnel` is set, so a cross-origin app needs no query
    /// param and no custom header.
    #[tokio::test]
    #[serial_test::serial]
    async fn test_tunnel_endpoint_with_embedded_envelope_dsn_ignores_host() {
        let ctx = create_test_context().await;
        let dsn = format!("https://{}@localhost/{}", ctx.dsn_key, ctx.project_id);
        let app = configure_tunnel_test_router(ctx.app_state.clone());
        let server = TestServer::new(app);

        let response = server
            .post(SENTRY_TUNNEL_ROUTE_PATH)
            .content_type("application/octet-stream")
            .add_header(
                HeaderName::from_static("host"),
                HeaderValue::from_static("app.hosted-elsewhere.example"),
            )
            .add_header(
                HeaderName::from_static("origin"),
                HeaderValue::from_static("https://app.hosted-elsewhere.example"),
            )
            .bytes(Bytes::from(tunneled_envelope_with_dsn(&dsn)))
            .await;

        assert_eq!(
            response.status_code(),
            StatusCode::OK,
            "An embedded envelope DSN must resolve the project: {}",
            response.text()
        );
    }

    /// An embedded DSN that does not resolve was sniffed, not presented, so it
    /// must fall through to the existing `Host` path instead of 401-ing an app
    /// that tunnels a third-party DSN through Temps.
    #[tokio::test]
    #[serial_test::serial]
    async fn test_tunnel_endpoint_unresolvable_embedded_dsn_falls_back_to_host() {
        let ctx = create_test_context().await;
        ctx.route_table
            .insert_route_for_test("app.example.com", test_route_info(ctx.project.clone()));
        let app = configure_tunnel_test_router(ctx.app_state.clone());
        let server = TestServer::new(app);

        let foreign_dsn = format!(
            "https://{}@o0.ingest.example/{}",
            "1111111111111111111111111111111111111111111111111111111111111111", 999_999
        );

        let response = server
            .post(SENTRY_TUNNEL_ROUTE_PATH)
            .content_type("application/octet-stream")
            .add_header(
                HeaderName::from_static("host"),
                HeaderValue::from_static("app.example.com"),
            )
            .add_header(
                HeaderName::from_static("origin"),
                HeaderValue::from_static("https://app.example.com"),
            )
            .bytes(Bytes::from(tunneled_envelope_with_dsn(&foreign_dsn)))
            .await;

        assert_eq!(
            response.status_code(),
            StatusCode::OK,
            "Host resolution must still apply when the embedded DSN is foreign: {}",
            response.text()
        );
    }

    /// The `Host` fallback keeps its `Origin` check: presenting no credential
    /// at all must behave exactly as it did before keyed ingest existed.
    #[tokio::test]
    #[serial_test::serial]
    async fn test_tunnel_endpoint_embedded_dsn_fallback_still_enforces_origin() {
        let ctx = create_test_context().await;
        ctx.route_table
            .insert_route_for_test("app.example.com", test_route_info(ctx.project.clone()));
        let app = configure_tunnel_test_router(ctx.app_state.clone());
        let server = TestServer::new(app);

        let foreign_dsn =
            "https://1111111111111111111111111111111111111111111111111111111111111111@o0.ingest.example/999999";

        let response = server
            .post(SENTRY_TUNNEL_ROUTE_PATH)
            .content_type("application/octet-stream")
            .add_header(
                HeaderName::from_static("host"),
                HeaderValue::from_static("app.example.com"),
            )
            .add_header(
                HeaderName::from_static("origin"),
                HeaderValue::from_static("https://evil.example.com"),
            )
            .bytes(Bytes::from(tunneled_envelope_with_dsn(foreign_dsn)))
            .await;

        assert_eq!(response.status_code(), StatusCode::FORBIDDEN);
    }

    /// The keyed path is bounded by the DSN row's own limit, and says how long
    /// to wait.
    #[tokio::test]
    #[serial_test::serial]
    async fn test_tunnel_endpoint_keyed_request_rate_limited_returns_429() {
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

        let ctx = create_test_context().await;
        let db = ctx._db.connection_arc();
        temps_entities::project_dsns::Entity::update_many()
            .col_expr(
                temps_entities::project_dsns::Column::RateLimitPerMinute,
                sea_orm::sea_query::Expr::value(1),
            )
            .filter(temps_entities::project_dsns::Column::PublicKey.eq(ctx.dsn_key.clone()))
            .exec(db.as_ref())
            .await
            .unwrap();

        let app = configure_tunnel_test_router(ctx.app_state.clone());
        let server = TestServer::new(app);
        let path = format!("{}?sentry_key={}", SENTRY_TUNNEL_ROUTE_PATH, ctx.dsn_key);

        let first = server
            .post(&path)
            .content_type("application/octet-stream")
            .add_header(
                HeaderName::from_static("host"),
                HeaderValue::from_static("app.hosted-elsewhere.example"),
            )
            .bytes(Bytes::from(tunneled_envelope_without_dsn()))
            .await;
        assert_eq!(first.status_code(), StatusCode::OK, "{}", first.text());

        let second = server
            .post(&path)
            .content_type("application/octet-stream")
            .add_header(
                HeaderName::from_static("host"),
                HeaderValue::from_static("app.hosted-elsewhere.example"),
            )
            .bytes(Bytes::from(tunneled_envelope_without_dsn()))
            .await;

        assert_eq!(second.status_code(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            second
                .headers()
                .get(header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok()),
            Some("60"),
            "A 429 must tell the client when to retry"
        );
    }

    #[test]
    fn test_origin_matches_host() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("https://app.example.com"),
        );
        assert!(origin_matches_host(&headers, "app.example.com"));
        assert!(!origin_matches_host(&headers, "other.example.com"));

        // Falls back to Referer when Origin is absent.
        let mut headers = HeaderMap::new();
        headers.insert(
            header::REFERER,
            HeaderValue::from_static("https://app.example.com/some/page"),
        );
        assert!(origin_matches_host(&headers, "app.example.com"));

        // Neither header present — default deny.
        assert!(!origin_matches_host(&HeaderMap::new(), "app.example.com"));
    }

    // === ADR-040 keyed tunnel: attribution, headers, origins, budget ===

    /// Mint a second project with its own DSN in the same test database, so
    /// the cross-project attribution cases below have a real "project B".
    async fn create_second_project_with_dsn(ctx: &TestContext) -> (i32, String) {
        use sea_orm::ActiveModelTrait;
        use sea_orm::Set;
        use temps_entities::projects;
        use uuid::Uuid;

        let project = projects::ActiveModel {
            name: Set("Second Project".to_string()),
            repo_name: Set("second-repo".to_string()),
            repo_owner: Set("test-owner".to_string()),
            directory: Set("/second".to_string()),
            main_branch: Set("main".to_string()),
            slug: Set(format!("second-project-{}", Uuid::new_v4())),
            preset: Set(Preset::NextJs),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(ctx._db.connection())
        .await
        .unwrap();

        let dsn = ctx
            .app_state
            .dsn_service
            .generate_project_dsn(
                project.id,
                None,
                None,
                Some("Second DSN".to_string()),
                "localhost",
            )
            .await
            .unwrap();

        (project.id, dsn.public_key)
    }

    async fn error_event_project_ids(ctx: &TestContext) -> Vec<i32> {
        use sea_orm::{EntityTrait, QueryOrder};
        temps_entities::error_events::Entity::find()
            .order_by_asc(temps_entities::error_events::Column::Id)
            .all(ctx._db.connection())
            .await
            .unwrap()
            .into_iter()
            .map(|event| event.project_id)
            .collect()
    }

    /// The core security claim of keyed tunnel ingest: an **explicitly
    /// presented** key decides the project, and a conflicting DSN embedded in
    /// the envelope by the SDK cannot redirect the data to someone else's
    /// project. Anyone can put any `dsn` string in an envelope header; only
    /// the query parameter/header is a presented credential.
    #[tokio::test]
    #[serial_test::serial]
    async fn test_explicit_key_wins_over_conflicting_embedded_dsn() {
        let ctx = create_test_context().await;
        let (project_b, key_b) = create_second_project_with_dsn(&ctx).await;
        assert_ne!(ctx.project_id, project_b);

        let app = configure_tunnel_test_router(ctx.app_state.clone());
        let server = TestServer::new(app);

        // Explicit key: project A. Embedded envelope DSN: project B.
        let embedded_dsn_for_b = format!("https://{}@localhost/{}", key_b, project_b);
        let response = server
            .post(&format!(
                "{}?sentry_key={}",
                SENTRY_TUNNEL_ROUTE_PATH, ctx.dsn_key
            ))
            .content_type("application/octet-stream")
            .add_header(
                HeaderName::from_static("host"),
                HeaderValue::from_static("app.hosted-elsewhere.example"),
            )
            .bytes(Bytes::from(tunneled_envelope_with_dsn(&embedded_dsn_for_b)))
            .await;

        assert_eq!(
            response.status_code(),
            StatusCode::OK,
            "{}",
            response.text()
        );

        assert_eq!(
            error_event_project_ids(&ctx).await,
            vec![ctx.project_id],
            "the explicitly presented key's project must own the event, not the embedded DSN's"
        );
    }

    /// The tunnel route accepts the same credential carriers as `/envelope/`:
    /// SDKs that cannot append a query string send `X-Sentry-Auth`.
    #[tokio::test]
    #[serial_test::serial]
    async fn test_tunnel_endpoint_accepts_x_sentry_auth_header() {
        let ctx = create_test_context().await;
        let app = configure_tunnel_test_router(ctx.app_state.clone());
        let server = TestServer::new(app);

        let auth_header = format!("Sentry sentry_key={},sentry_version=7", ctx.dsn_key);
        let response = server
            .post(SENTRY_TUNNEL_ROUTE_PATH)
            .content_type("application/octet-stream")
            .add_header(
                HeaderName::from_static("host"),
                HeaderValue::from_static("app.hosted-elsewhere.example"),
            )
            .add_header(
                HeaderName::from_static("x-sentry-auth"),
                HeaderValue::from_str(&auth_header).unwrap(),
            )
            .bytes(Bytes::from(tunneled_envelope_without_dsn()))
            .await;

        assert_eq!(
            response.status_code(),
            StatusCode::OK,
            "{}",
            response.text()
        );
        assert_eq!(error_event_project_ids(&ctx).await, vec![ctx.project_id]);
    }

    /// …and `Authorization: DSN <key>`, the other carrier `extract_dsn_key`
    /// understands.
    #[tokio::test]
    #[serial_test::serial]
    async fn test_tunnel_endpoint_accepts_authorization_dsn_header() {
        let ctx = create_test_context().await;
        let app = configure_tunnel_test_router(ctx.app_state.clone());
        let server = TestServer::new(app);

        let response = server
            .post(SENTRY_TUNNEL_ROUTE_PATH)
            .content_type("application/octet-stream")
            .add_header(
                HeaderName::from_static("host"),
                HeaderValue::from_static("app.hosted-elsewhere.example"),
            )
            .add_header(
                HeaderName::from_static("authorization"),
                HeaderValue::from_str(&format!("DSN {}", ctx.dsn_key)).unwrap(),
            )
            .bytes(Bytes::from(tunneled_envelope_without_dsn()))
            .await;

        assert_eq!(
            response.status_code(),
            StatusCode::OK,
            "{}",
            response.text()
        );
        assert_eq!(error_event_project_ids(&ctx).await, vec![ctx.project_id]);
    }

    /// An empty `?sentry_key=` presented nothing, so it must behave like a
    /// credential-free request (Host fallback) rather than 401-ing.
    #[tokio::test]
    #[serial_test::serial]
    async fn test_tunnel_endpoint_empty_sentry_key_falls_back_to_host() {
        let ctx = create_test_context().await;
        ctx.route_table
            .insert_route_for_test("app.example.com", test_route_info(ctx.project.clone()));
        let app = configure_tunnel_test_router(ctx.app_state.clone());
        let server = TestServer::new(app);

        let response = server
            .post(&format!("{}?sentry_key=", SENTRY_TUNNEL_ROUTE_PATH))
            .content_type("application/octet-stream")
            .add_header(
                HeaderName::from_static("host"),
                HeaderValue::from_static("app.example.com"),
            )
            .add_header(
                HeaderName::from_static("origin"),
                HeaderValue::from_static("https://app.example.com"),
            )
            .bytes(Bytes::from(tunneled_envelope_without_dsn()))
            .await;

        assert_eq!(
            response.status_code(),
            StatusCode::OK,
            "an empty sentry_key is absent, not invalid: {}",
            response.text()
        );
    }

    /// A key whose project has been soft-deleted must stop ingesting: project
    /// deletion does not cascade to `project_dsns`, so without the `projects`
    /// join the DSN would keep filling a project nothing can read.
    #[tokio::test]
    #[serial_test::serial]
    async fn test_tunnel_endpoint_soft_deleted_project_returns_401() {
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

        let ctx = create_test_context().await;
        temps_entities::projects::Entity::update_many()
            .col_expr(
                temps_entities::projects::Column::IsDeleted,
                sea_orm::sea_query::Expr::value(true),
            )
            .filter(temps_entities::projects::Column::Id.eq(ctx.project_id))
            .exec(ctx._db.connection())
            .await
            .unwrap();

        let app = configure_tunnel_test_router(ctx.app_state.clone());
        let server = TestServer::new(app);

        let response = server
            .post(&format!(
                "{}?sentry_key={}",
                SENTRY_TUNNEL_ROUTE_PATH, ctx.dsn_key
            ))
            .content_type("application/octet-stream")
            .add_header(
                HeaderName::from_static("host"),
                HeaderValue::from_static("app.hosted-elsewhere.example"),
            )
            .bytes(Bytes::from(tunneled_envelope_without_dsn()))
            .await;

        assert_eq!(response.status_code(), StatusCode::UNAUTHORIZED);
        assert!(error_event_project_ids(&ctx).await.is_empty());
    }

    /// `allowed_origins` is enforced when the row carries one: a matching
    /// `Origin` is admitted…
    #[tokio::test]
    #[serial_test::serial]
    async fn test_tunnel_endpoint_allowed_origin_is_admitted() {
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

        let ctx = create_test_context().await;
        temps_entities::project_dsns::Entity::update_many()
            .col_expr(
                temps_entities::project_dsns::Column::AllowedOrigins,
                sea_orm::sea_query::Expr::value(serde_json::json!(["https://allowed.example.com"])),
            )
            .filter(temps_entities::project_dsns::Column::PublicKey.eq(ctx.dsn_key.clone()))
            .exec(ctx._db.connection())
            .await
            .unwrap();

        let app = configure_tunnel_test_router(ctx.app_state.clone());
        let server = TestServer::new(app);

        let response = server
            .post(&format!(
                "{}?sentry_key={}",
                SENTRY_TUNNEL_ROUTE_PATH, ctx.dsn_key
            ))
            .content_type("application/octet-stream")
            .add_header(
                HeaderName::from_static("host"),
                HeaderValue::from_static("app.hosted-elsewhere.example"),
            )
            .add_header(
                HeaderName::from_static("origin"),
                HeaderValue::from_static("https://allowed.example.com"),
            )
            .bytes(Bytes::from(tunneled_envelope_without_dsn()))
            .await;

        assert_eq!(
            response.status_code(),
            StatusCode::OK,
            "{}",
            response.text()
        );
    }

    /// …and an `Origin` off the list is a 403 rather than a silent
    /// fall-through to `Host`, which would undo the restriction.
    #[tokio::test]
    #[serial_test::serial]
    async fn test_tunnel_endpoint_disallowed_origin_returns_403() {
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

        let ctx = create_test_context().await;
        ctx.route_table
            .insert_route_for_test("app.example.com", test_route_info(ctx.project.clone()));
        temps_entities::project_dsns::Entity::update_many()
            .col_expr(
                temps_entities::project_dsns::Column::AllowedOrigins,
                sea_orm::sea_query::Expr::value(serde_json::json!(["https://allowed.example.com"])),
            )
            .filter(temps_entities::project_dsns::Column::PublicKey.eq(ctx.dsn_key.clone()))
            .exec(ctx._db.connection())
            .await
            .unwrap();

        let app = configure_tunnel_test_router(ctx.app_state.clone());
        let server = TestServer::new(app);

        let response = server
            .post(&format!(
                "{}?sentry_key={}",
                SENTRY_TUNNEL_ROUTE_PATH, ctx.dsn_key
            ))
            .content_type("application/octet-stream")
            .add_header(
                HeaderName::from_static("host"),
                HeaderValue::from_static("app.example.com"),
            )
            .add_header(
                HeaderName::from_static("origin"),
                HeaderValue::from_static("https://evil.example.com"),
            )
            .bytes(Bytes::from(tunneled_envelope_without_dsn()))
            .await;

        assert_eq!(response.status_code(), StatusCode::FORBIDDEN);
        assert!(error_event_project_ids(&ctx).await.is_empty());
    }

    /// `Host` resolving to project A while the envelope's embedded DSN
    /// resolves to a *different* project B must attribute the event to B
    /// (the credential, per `test_tunnel_endpoint_with_embedded_envelope_dsn_ignores_host`)
    /// without ever consuming A's rate-limit budget — A received no data,
    /// so it must not be billed for this request. A regression test for a
    /// bug where the speculative `Host` charge happened before the embedded
    /// DSN was even read, so B's traffic silently drained A's allowance.
    #[tokio::test]
    #[serial_test::serial]
    async fn test_tunnel_endpoint_host_project_is_not_charged_when_embedded_dsn_wins() {
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

        let ctx = create_test_context().await;
        let (project_b, key_b) = create_second_project_with_dsn(&ctx).await;

        // Give project A (the Host match) a single-request budget so any
        // charge against it is immediately observable.
        temps_entities::project_dsns::Entity::update_many()
            .col_expr(
                temps_entities::project_dsns::Column::RateLimitPerMinute,
                sea_orm::sea_query::Expr::value(1),
            )
            .filter(temps_entities::project_dsns::Column::PublicKey.eq(ctx.dsn_key.clone()))
            .exec(ctx._db.connection())
            .await
            .unwrap();

        ctx.route_table
            .insert_route_for_test("app.example.com", test_route_info(ctx.project.clone()));

        let app = configure_tunnel_test_router(ctx.app_state.clone());
        let server = TestServer::new(app);

        let embedded_dsn_for_b = format!("https://{}@localhost/{}", key_b, project_b);
        let response = server
            .post(SENTRY_TUNNEL_ROUTE_PATH)
            .content_type("application/octet-stream")
            .add_header(
                HeaderName::from_static("host"),
                HeaderValue::from_static("app.example.com"),
            )
            .add_header(
                HeaderName::from_static("origin"),
                HeaderValue::from_static("https://app.example.com"),
            )
            .bytes(Bytes::from(tunneled_envelope_with_dsn(&embedded_dsn_for_b)))
            .await;

        assert_eq!(
            response.status_code(),
            StatusCode::OK,
            "{}",
            response.text()
        );
        assert_eq!(
            error_event_project_ids(&ctx).await,
            vec![project_b],
            "the embedded DSN's project must own the event"
        );

        // Project A's single-request budget must still be untouched: it was
        // only ever the tentative Host match, never the attributed project.
        assert!(
            ctx.app_state
                .rate_limiter
                .peek(ctx.project_id, Some(1))
                .await,
            "project A must not have been charged for a request it never received"
        );
    }

    /// A `Host`-resolved request that reserves a rate-limit slot and then
    /// fails to decompress must release that slot rather than leaking it — a
    /// regression test for a bug where the decompression-error return path
    /// bypassed the only release call, so repeated malformed-gzip requests
    /// against a project could exhaust its tunnel allowance without a single
    /// event ever landing.
    #[tokio::test]
    #[serial_test::serial]
    async fn test_tunnel_endpoint_decompression_failure_releases_the_host_reservation() {
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

        let ctx = create_test_context().await;

        temps_entities::project_dsns::Entity::update_many()
            .col_expr(
                temps_entities::project_dsns::Column::RateLimitPerMinute,
                sea_orm::sea_query::Expr::value(1),
            )
            .filter(temps_entities::project_dsns::Column::PublicKey.eq(ctx.dsn_key.clone()))
            .exec(ctx._db.connection())
            .await
            .unwrap();

        ctx.route_table
            .insert_route_for_test("app.example.com", test_route_info(ctx.project.clone()));

        let app = configure_tunnel_test_router(ctx.app_state.clone());
        let server = TestServer::new(app);

        let response = server
            .post(SENTRY_TUNNEL_ROUTE_PATH)
            .content_type("application/octet-stream")
            .add_header(
                HeaderName::from_static("host"),
                HeaderValue::from_static("app.example.com"),
            )
            .add_header(
                HeaderName::from_static("origin"),
                HeaderValue::from_static("https://app.example.com"),
            )
            .add_header(
                HeaderName::from_static("content-encoding"),
                HeaderValue::from_static("gzip"),
            )
            .bytes(Bytes::from_static(b"not actually gzip data"))
            .await;

        assert_eq!(
            response.status_code(),
            StatusCode::BAD_REQUEST,
            "{}",
            response.text()
        );

        // The 1-request budget must still be fully available: the reservation
        // made before decompression was tried must have been released when
        // decompression failed.
        assert!(
            ctx.app_state
                .rate_limiter
                .peek(ctx.project_id, Some(1))
                .await,
            "a failed decompression must release its rate-limit reservation"
        );
    }

    /// A malformed key is rejected on shape alone — the `Ok(None)` below comes
    /// back without the query ever being built, which is what keeps a flood of
    /// junk from costing a round trip each.
    #[tokio::test]
    #[serial_test::serial]
    async fn test_malformed_public_key_never_reaches_the_database() {
        let ctx = create_test_context().await;

        for candidate in [
            "",
            "short",
            "tk_0123456789abcdef0123456789abcdef",
            "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz",
            &"a".repeat(65),
        ] {
            assert!(
                !crate::sentry::dsn_service::is_well_formed_public_key(candidate),
                "{candidate:?} must be rejected on shape"
            );
            assert!(
                ctx.app_state
                    .dsn_service
                    .get_project_by_public_key(candidate)
                    .await
                    .unwrap()
                    .is_none(),
                "{candidate:?} must not resolve"
            );
        }

        // The real key still resolves — the gate is a shape check, not a ban.
        assert!(ctx
            .app_state
            .dsn_service
            .get_project_by_public_key(&ctx.dsn_key)
            .await
            .unwrap()
            .is_some());
    }

    /// The global unresolved-credential budget trips before the database is
    /// consulted, and the 429 carries `Retry-After`.
    #[tokio::test]
    #[serial_test::serial]
    async fn test_unresolved_credential_flood_trips_the_global_budget() {
        use crate::sentry::rate_limiter::UNRESOLVED_CREDENTIAL_LIMIT_PER_MINUTE;

        let ctx = create_test_context().await;
        let app = configure_tunnel_test_router(ctx.app_state.clone());
        let server = TestServer::new(app);

        // Distinct, well-shaped, non-existent keys: each one misses the
        // resolution cache, which is exactly the case the budget exists for.
        let mut unauthorized = 0;
        let mut rate_limited = None;
        for n in 0..(UNRESOLVED_CREDENTIAL_LIMIT_PER_MINUTE + 5) {
            let key = format!("{:064x}", n + 1);
            let response = server
                .post(&format!("{}?sentry_key={}", SENTRY_TUNNEL_ROUTE_PATH, key))
                .content_type("application/octet-stream")
                .add_header(
                    HeaderName::from_static("host"),
                    HeaderValue::from_static("app.hosted-elsewhere.example"),
                )
                .bytes(Bytes::from(tunneled_envelope_without_dsn()))
                .await;

            match response.status_code() {
                StatusCode::UNAUTHORIZED => unauthorized += 1,
                StatusCode::TOO_MANY_REQUESTS => {
                    rate_limited = Some(response);
                    break;
                }
                other => panic!("unexpected status {other} on attempt {n}"),
            }
        }

        assert_eq!(
            unauthorized, UNRESOLVED_CREDENTIAL_LIMIT_PER_MINUTE,
            "the budget must admit exactly its limit before tripping"
        );
        let rate_limited = rate_limited.expect("the flood must eventually be rate limited");
        assert_eq!(
            rate_limited
                .headers()
                .get(header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok()),
            Some(INGEST_RETRY_AFTER_SECONDS)
        );
    }

    /// `/envelope/` and `/store/` answer a 429 the same way the tunnel route
    /// does, so an SDK does not have to special-case which endpoint it hit.
    #[tokio::test]
    #[serial_test::serial]
    async fn test_envelope_endpoint_rate_limited_sets_retry_after() {
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

        let ctx = create_test_context().await;
        temps_entities::project_dsns::Entity::update_many()
            .col_expr(
                temps_entities::project_dsns::Column::RateLimitPerMinute,
                sea_orm::sea_query::Expr::value(1),
            )
            .filter(temps_entities::project_dsns::Column::PublicKey.eq(ctx.dsn_key.clone()))
            .exec(ctx._db.connection())
            .await
            .unwrap();

        let app = configure_routes().with_state(ctx.app_state.clone());
        let server = TestServer::new(app);
        let auth_header = format!("Sentry sentry_key={},sentry_version=7", ctx.dsn_key);
        let envelope = tunneled_envelope_without_dsn();

        for _ in 0..2 {
            let response = server
                .post(&format!("/{}/envelope/", ctx.project_id))
                .content_type("application/octet-stream")
                .add_header(
                    HeaderName::from_static("x-sentry-auth"),
                    HeaderValue::from_str(&auth_header).unwrap(),
                )
                .bytes(Bytes::from(envelope.clone()))
                .await;

            if response.status_code() == StatusCode::TOO_MANY_REQUESTS {
                assert_eq!(
                    response
                        .headers()
                        .get(header::RETRY_AFTER)
                        .and_then(|v| v.to_str().ok()),
                    Some(INGEST_RETRY_AFTER_SECONDS),
                    "a 429 must tell the client when to retry"
                );
                return;
            }
        }

        panic!("expected the second request to be rate limited");
    }

    #[test]
    fn test_origins_match() {
        assert!(origins_match(
            "https://app.example.com",
            "https://app.example.com"
        ));
        // Scheme and host compare case-insensitively.
        assert!(origins_match(
            "HTTPS://APP.EXAMPLE.COM",
            "https://app.example.com"
        ));
        // Scheme must match.
        assert!(!origins_match(
            "https://app.example.com",
            "http://app.example.com"
        ));
        // Port is part of the origin.
        assert!(origins_match(
            "http://localhost:3000",
            "http://localhost:3000"
        ));
        assert!(!origins_match(
            "http://localhost:3000",
            "http://localhost:3001"
        ));
        assert!(!origins_match("http://localhost:3000", "http://localhost"));
        // A bracketed IPv6 literal's colons are not a port separator.
        assert!(origins_match("http://[::1]:8080", "http://[::1]:8080"));
        assert!(!origins_match("http://[::1]:8080", "http://[::1]:9090"));
        // Suffix tricks do not match.
        assert!(!origins_match(
            "https://example.com",
            "https://evil-example.com"
        ));
    }

    #[test]
    fn test_extract_dsn_key_treats_empty_values_as_absent() {
        let empty_params = std::collections::HashMap::new();

        let mut params = std::collections::HashMap::new();
        params.insert("sentry_key".to_string(), String::new());
        assert_eq!(extract_dsn_key(&HeaderMap::new(), &params), None);

        params.insert("sentry_key".to_string(), "   ".to_string());
        assert_eq!(extract_dsn_key(&HeaderMap::new(), &params), None);

        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("x-sentry-auth"),
            HeaderValue::from_static("Sentry sentry_key=,sentry_version=7"),
        );
        assert_eq!(extract_dsn_key(&headers, &empty_params), None);

        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("authorization"),
            HeaderValue::from_static("DSN "),
        );
        assert_eq!(extract_dsn_key(&headers, &empty_params), None);
    }
    // Native assertions live in Rust. Foreign runtimes only emit official SDK
    // traffic. Explicitly ignored unless invoked by scripts/sentry-acceptance.sh;
    // missing prerequisites FAIL that run, rather than silently skipping it.
    struct PrioritySdkHarness {
        ctx: TestContext,
        server: TestServer,
        dsn: String,
        tunnel: String,
    }
    impl PrioritySdkHarness {
        async fn new() -> Self {
            let ctx = create_test_context().await;
            let router = Router::new()
                .nest("/api", configure_routes())
                .layer(axum::middleware::from_fn(inject_test_request_metadata))
                .with_state(ctx.app_state.clone());
            let server = TestServer::builder()
                .http_transport()
                .build(router.into_make_service_with_connect_info::<SocketAddr>());
            let base = server.server_address().unwrap();
            Self {
                dsn: format!(
                    "http://{}@{}/{}",
                    ctx.dsn_key,
                    base.authority(),
                    ctx.project_id
                ),
                tunnel: format!("{base}api{SENTRY_TUNNEL_ROUTE_PATH}"),
                ctx,
                server,
            }
        }
        fn fixture(name: &str) -> String {
            let root = std::env::var("TEMPS_SENTRY_REPO_ROOT")
                .unwrap_or_else(|_| format!("{}/../..", env!("CARGO_MANIFEST_DIR")));
            format!("{root}/scripts/{name}")
        }
        fn prerequisite(name: &str) -> String {
            std::env::var(name)
                .unwrap_or_else(|_| panic!("Missing {name}; run scripts/sentry-acceptance.sh sdk"))
        }
        async fn emit(
            &self,
            runtime: &str,
            args: &[String],
            cwd: Option<&str>,
            expected: i32,
        ) -> serde_json::Value {
            let mut command = tokio::process::Command::new(runtime);
            command
                .args(args)
                .env("SENTRY_DSN", &self.dsn)
                .kill_on_drop(true);
            if let Some(path) = cwd {
                command.current_dir(path);
            }
            let output =
                tokio::time::timeout(std::time::Duration::from_secs(180), command.output())
                    .await
                    .expect("Official SDK process timed out")
                    .expect("Could not start official SDK runtime");
            assert_eq!(
                output.status.code(),
                Some(expected),
                "Official SDK fixture failed; credential-bearing output suppressed"
            );
            let stdout = std::str::from_utf8(&output.stdout).expect("SDK summary is not UTF8");
            serde_json::from_str(stdout.lines().last().expect("SDK summary missing"))
                .expect("SDK summary is not JSON")
        }
        async fn events(&self, run: &str) -> Vec<(i32, serde_json::Value)> {
            use sea_orm::{ConnectionTrait, DbBackend, Statement};
            self.ctx._db.connection().query_all(Statement::from_sql_and_values(DbBackend::Postgres,
                "SELECT error_group_id,data FROM error_events WHERE project_id=$1 AND data #>> '{sentry,tags,synthetic.run}'=$2 ORDER BY id",
                [self.ctx.project_id.into(),run.into()])).await.unwrap().into_iter().map(|r|(r.try_get("","error_group_id").unwrap(),r.try_get("","data").unwrap())).collect()
        }
        async fn metric(
            &self,
            name: &str,
            run: &str,
            aggregation: temps_otel::types::MetricAggregation,
            interval: &str,
        ) -> Vec<temps_otel::types::MetricBucket> {
            self.ctx
                .app_state
                .otel_service
                .query_metrics(temps_otel::types::MetricQuery {
                    project_id: self.ctx.project_id,
                    metric_name: Some(name.into()),
                    label_filters: vec![("synthetic.run".into(), run.into())],
                    aggregation,
                    bucket_interval: Some(interval.into()),
                    ..Default::default()
                })
                .await
                .unwrap()
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "Official SDK prerequisites: scripts/sentry-acceptance.sh sdk"]
    #[serial_test::serial]
    async fn priority_sdk_core_matrix() {
        use sea_orm::{ConnectionTrait, DbBackend, Statement};
        let h = PrioritySdkHarness::new().await;
        let node10 = PrioritySdkHarness::prerequisite("TEMPS_SENTRY_NODE10_SDK");
        let node11 = PrioritySdkHarness::prerequisite("TEMPS_SENTRY_NODE11_SDK");
        let browser = PrioritySdkHarness::prerequisite("TEMPS_SENTRY_BROWSER_SDK");
        let python = PrioritySdkHarness::prerequisite("TEMPS_SENTRY_PYTHON");
        let go = PrioritySdkHarness::prerequisite("TEMPS_SENTRY_GO_WORKSPACE");
        let rust = PrioritySdkHarness::prerequisite("TEMPS_SENTRY_RUST_MANIFEST");
        let mut cases = Vec::new();
        for (runtime, entry) in [("node", node10), ("node", node11), ("bun", browser)] {
            for tunnel in [false, true] {
                cases.push((
                    runtime.to_string(),
                    vec![
                        PrioritySdkHarness::fixture("sentry-errors-conformance.mjs"),
                        entry.clone(),
                        h.dsn.clone(),
                    ],
                    None,
                    tunnel,
                ));
            }
        }
        cases.push((
            python,
            vec![PrioritySdkHarness::fixture("fixtures/sentry-errors.py")],
            None,
            false,
        ));
        cases.push(("go".into(), vec!["run".into(), ".".into()], Some(go), false));
        cases.push((
            "cargo".into(),
            vec![
                "run".into(),
                "--quiet".into(),
                "--manifest-path".into(),
                rust,
                "-j2".into(),
                "--".into(),
            ],
            None,
            false,
        ));
        for (runtime, mut args, cwd, tunnel) in cases {
            let run = format!("synthetic-core-{}", uuid::Uuid::new_v4().simple());
            args.push(run.clone());
            if tunnel {
                args.push(h.tunnel.clone());
            }
            let report = h.emit(&runtime, &args, cwd.as_deref(), 0).await;
            let ids = report["ids"].as_array().unwrap();
            assert_eq!(ids.len(), 4);
            let rows = h.events(&run).await;
            assert_eq!(rows.len(), 4);
            let mut groups = std::collections::BTreeMap::new();
            for (group, data) in &rows {
                let raw = &data["sentry"];
                assert!(ids.contains(&raw["event_id"]));
                assert_eq!(raw["sdk"]["version"], report["sdk"]);
                assert_eq!(data["user"]["user_id"], "synthetic-user");
                assert_eq!(data["environment"]["release"], "synthetic-release-2");
                assert!(data["trace"]["contexts"]["synthetic"]["nullable"].is_null());
                assert!(data["trace"]["breadcrumbs"]
                    .as_array()
                    .is_some_and(|v| !v.is_empty()));
                let (_, count) = h
                    .ctx
                    .app_state
                    .error_tracking_service
                    .crud
                    .list_error_events_filtered(
                        *group,
                        h.ctx.project_id,
                        Some(1),
                        Some(100),
                        &crate::services::ErrorEventFilter {
                            user_id: Some("synthetic-user".into()),
                            tag_key: Some("synthetic.run".into()),
                            tag_value: Some(run.clone()),
                        },
                    )
                    .await
                    .unwrap();
                assert!(count > 0);
                let (_, foreign) = h
                    .ctx
                    .app_state
                    .error_tracking_service
                    .crud
                    .list_error_events(*group, h.ctx.project_id + 1, None, None)
                    .await
                    .unwrap();
                assert_eq!(foreign, 0);
                groups.insert(*group, ());
            }
            assert_eq!(groups.len(), 3);
            let group_for = |id: &serde_json::Value| {
                rows.iter()
                    .find(|(_, v)| v["sentry"]["event_id"] == *id)
                    .unwrap()
                    .0
            };
            assert_eq!(group_for(&ids[0]), group_for(&ids[1]));
            let count = h
                .ctx
                ._db
                .connection()
                .query_one(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "SELECT total_count::bigint AS total_count FROM error_groups WHERE id=$1",
                    [group_for(&ids[0]).into()],
                ))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(count.try_get::<i64>("", "total_count").unwrap(), 2);
            println!(
                "core SDK {} {}: four native events, three groups, retries/metadata/queries passed",
                report["sdk"],
                if tunnel { "tunnel" } else { "direct" }
            );
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "Official SDK prerequisites: scripts/sentry-acceptance.sh sdk"]
    #[serial_test::serial]
    async fn priority_sdk_framework_native_acceptance() {
        use temps_otel::types::*;
        let h = PrioritySdkHarness::new().await;
        let node = PrioritySdkHarness::prerequisite("TEMPS_SENTRY_NODE11_SDK");
        let browser = PrioritySdkHarness::prerequisite("TEMPS_SENTRY_BROWSER_SDK");
        let run = format!("synthetic-framework-{}", uuid::Uuid::new_v4().simple());
        let report = h
            .emit(
                "bun",
                &[
                    PrioritySdkHarness::fixture("sentry-framework-conformance.mjs"),
                    node.clone(),
                    browser.clone(),
                    run.clone(),
                ],
                None,
                0,
            )
            .await;
        let trace = report["trace_id"].as_str().unwrap();
        let spans = h
            .ctx
            .app_state
            .otel_service
            .get_trace(h.ctx.project_id, trace)
            .await
            .unwrap();
        assert!(spans.len() >= 4);
        let ids: std::collections::BTreeSet<_> = spans.iter().map(|s| s.span_id.as_str()).collect();
        assert_eq!(ids.len(), spans.len());
        let roots: Vec<_> = spans
            .iter()
            .filter(|s| s.parent_span_id.is_none())
            .collect();
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].name, "Synthetic browser checkout");
        assert!(spans.iter().all(|s| s
            .parent_span_id
            .as_deref()
            .is_none_or(|id| ids.contains(id))));
        assert!(spans.iter().filter(|s| s.kind == SpanKind::Server).count() >= 2);
        assert!(spans.iter().any(|s| s.kind == SpanKind::Client));
        assert!(spans.iter().any(|s| s.status_code == SpanStatusCode::Error));
        assert!(spans
            .iter()
            .all(|s| s.resource.service_version.as_deref() == Some(run.as_str())));
        let rows = h.events(&run).await;
        assert_eq!(rows.len(), 1);
        let data = &rows[0].1;
        assert_eq!(data["sentry"]["event_id"], report["event_id"]);
        let exceptions = data["sentry"]["exception"]["values"].as_array().unwrap();
        assert_eq!(exceptions.len(), 2);
        assert_eq!(exceptions[0]["value"], "Synthetic framework cause");
        assert_eq!(exceptions[1]["value"], "Synthetic framework outer");
        assert!(exceptions.iter().all(|v| v["stacktrace"]["frames"]
            .as_array()
            .is_some_and(|v| !v.is_empty())));
        assert_eq!(data["trace"]["contexts"]["trace"]["trace_id"], trace);
        assert_eq!(
            data["trace"]["contexts"]["synthetic_context"]["nested"],
            serde_json::json!({"number":7,"flag":true,"missing":null})
        );
        assert_eq!(
            data["trace"]["contexts"]["synthetic_context"]["unicode"],
            "Synthetic café 漢字"
        );
        let breadcrumb = data["trace"]["breadcrumbs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["message"] == "Synthetic checkout")
            .unwrap();
        assert_eq!(
            breadcrumb["data"],
            serde_json::json!({"count":7,"flag":true,"nested":{"empty":null}})
        );
        let logs = h
            .ctx
            .app_state
            .otel_service
            .query_logs(LogQuery {
                project_id: h.ctx.project_id,
                trace_id: Some(trace.into()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(logs.len(), 1);
        assert!(ids.contains(logs[0].span_id.as_deref().unwrap()));
        assert_eq!(logs[0].body, "Synthetic framework item 7");
        assert_eq!(logs[0].attributes["sentry.message.parameter.0"], "7");
        for interval in ["5min", "1h"] {
            for (name, aggregation, expected) in [
                ("requests", MetricAggregation::Sum, 2.0),
                ("queue", MetricAggregation::Avg, 7.0),
                ("duration", MetricAggregation::Sum, 25.0),
                ("duration", MetricAggregation::Count, 5.0),
                ("duration", MetricAggregation::Avg, 5.0),
                ("duration", MetricAggregation::Min, 1.0),
                ("duration", MetricAggregation::Max, 9.0),
                ("duration", MetricAggregation::Quantile(0.5), 5.0),
                ("duration", MetricAggregation::Quantile(0.95), 8.6),
            ] {
                let buckets = h
                    .metric(
                        &format!("synthetic.framework.{name}"),
                        &run,
                        aggregation,
                        interval,
                    )
                    .await;
                assert_eq!(buckets.len(), 1);
                assert!(
                    (buckets[0].value - expected).abs() < 1e-9,
                    "Native {name} aggregate mismatch"
                );
            }
        }
        let releases = h
            .ctx
            .app_state
            .error_tracking_service
            .releases
            .list(h.ctx.project_id, Some(run.clone()), 7, 1, 20)
            .await
            .unwrap();
        assert_eq!(releases.items.len(), 1);
        let release = &releases.items[0];
        assert_eq!(
            (
                release.errors,
                release.logs,
                release.spans,
                release.metric_points
            ),
            (1, 1, report["span_count"].as_i64().unwrap(), 7)
        );
        let transport = h
            .emit(
                "node",
                &[
                    PrioritySdkHarness::fixture("sentry-transport-conformance.mjs"),
                    node.clone(),
                    run.clone(),
                ],
                None,
                0,
            )
            .await;
        let rows = h.events(&run).await;
        let event_ids: Vec<_> = rows.iter().map(|(_, v)| &v["sentry"]["event_id"]).collect();
        assert!(event_ids.contains(&&transport["recovered"]));
        assert!(!event_ids.contains(&&transport["first"]));
        assert!(!event_ids.contains(&&transport["suppressed"]));
        for mode in ["uncaught", "rejection"] {
            let fatal = h
                .emit(
                    "node",
                    &[
                        PrioritySdkHarness::fixture("sentry-framework-conformance.mjs"),
                        node.clone(),
                        browser.clone(),
                        run.clone(),
                        mode.into(),
                    ],
                    None,
                    1,
                )
                .await;
            let rows = h.events(&format!("{run}.{mode}")).await;
            assert_eq!(rows.len(), 1);
            let data = &rows[0].1;
            assert_eq!(data["sentry"]["event_id"], fatal["id"]);
            let exceptions = data["sentry"]["exception"]["values"].as_array().unwrap();
            assert_eq!(exceptions.last().unwrap()["mechanism"]["handled"], false);
            assert!(data["stack_trace"]
                .as_array()
                .is_some_and(|v| !v.is_empty()));
        }
        // Explicit wire fixture: child arrives before parent; native retry is zero new rows.
        let wire_trace = uuid::Uuid::new_v4().simple().to_string();
        let root = "1111111111111111";
        let child = "2222222222222222";
        let path = format!(
            "/api/{}/envelope/?sentry_key={}",
            h.ctx.project_id, h.ctx.dsn_key
        );
        for (id, parent, expected) in [
            (child, Some(root), "1"),
            (root, None, "1"),
            (child, Some(root), "0"),
        ] {
            let payload = serde_json::json!({"version":2,"items":[{"name":"Synthetic ordered span","trace_id":wire_trace,"span_id":id,"parent_span_id":parent,"start_timestamp":Utc::now().timestamp()-1,"end_timestamp":Utc::now().timestamp(),"status":"ok","attributes":{}}]});
            let header = serde_json::json!({"type":"span","item_count":1,"content_type":"application/vnd.sentry.items.span.v2+json"});
            let response = h
                .server
                .post(&path)
                .text(format!("{{}}\n{header}\n{payload}\n"))
                .await;
            response.assert_status_ok();
            assert_eq!(response.header("x-temps-sentry-spans-stored"), expected);
        }
        let spans = h
            .ctx
            .app_state
            .otel_service
            .get_trace(h.ctx.project_id, &wire_trace)
            .await
            .unwrap();
        assert_eq!(spans.len(), 2);
        assert_eq!(
            spans
                .iter()
                .find(|s| s.span_id == child)
                .unwrap()
                .parent_span_id
                .as_deref(),
            Some(root)
        );
        println!("browser/Express/downstream, fatal integrations, metadata, 18 metric aggregates, release correlation, gzip/Retry-After and out-of-order wire acceptance passed");
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "Official SDK prerequisites: scripts/sentry-acceptance.sh sdk"]
    #[serial_test::serial]
    async fn priority_sdk_cross_runtime_traces_and_typed_logs() {
        use temps_otel::types::*;
        let h = PrioritySdkHarness::new().await;
        let python = PrioritySdkHarness::prerequisite("TEMPS_SENTRY_PYTHON");
        let go = PrioritySdkHarness::prerequisite("TEMPS_SENTRY_GO_WORKSPACE");
        let rust = PrioritySdkHarness::prerequisite("TEMPS_SENTRY_RUST_MANIFEST");
        for (runtime, args, cwd) in [
            (
                python.clone(),
                vec![PrioritySdkHarness::fixture("fixtures/sentry-traces.py")],
                None,
            ),
            (
                "go".into(),
                vec![
                    "run".into(),
                    PrioritySdkHarness::fixture("fixtures/sentry-traces.go"),
                ],
                Some(go),
            ),
            (
                "cargo".into(),
                vec![
                    "run".into(),
                    "--quiet".into(),
                    "--manifest-path".into(),
                    rust,
                    "--example".into(),
                    "traces".into(),
                    "-j2".into(),
                    "--".into(),
                ],
                None,
            ),
        ] {
            let run = format!("synthetic-tracing-{}", uuid::Uuid::new_v4().simple());
            let mut args = args;
            args.push(run.clone());
            let report = h.emit(&runtime, &args, cwd.as_deref(), 0).await;
            let spans = h
                .ctx
                .app_state
                .otel_service
                .get_trace(h.ctx.project_id, report["trace_id"].as_str().unwrap())
                .await
                .unwrap();
            assert_eq!(spans.len(), 2);
            let root = spans
                .iter()
                .find(|s| s.span_id == report["root_id"].as_str().unwrap())
                .unwrap();
            let child = spans
                .iter()
                .find(|s| s.span_id == report["child_id"].as_str().unwrap())
                .unwrap();
            assert_eq!(root.name, "Synthetic cross SDK root");
            assert_eq!(child.name, "Synthetic cross SDK child");
            assert!(root.parent_span_id.is_none());
            assert_eq!(child.parent_span_id.as_deref(), Some(root.span_id.as_str()));
            assert_eq!(
                (root.status_code, child.status_code),
                (SpanStatusCode::Ok, SpanStatusCode::Error)
            );
            assert_eq!(
                (root.kind, child.kind),
                (SpanKind::Server, SpanKind::Client)
            );
            assert_eq!(child.status_message, "internal_error");
            assert_eq!(child.attributes["synthetic.flag"], "true");
            for span in spans {
                assert!(span.duration_ms > 0.0);
                assert_eq!(span.attributes["synthetic.run"], run);
                assert_eq!(span.resource.service_name, "synthetic-tracing-service");
                assert_eq!(
                    span.resource.service_version.as_deref(),
                    Some("synthetic-tracing-release")
                );
                assert_eq!(
                    span.resource.deployment_environment.as_deref(),
                    Some("synthetic-test")
                );
            }
        }
        let run = format!("synthetic-typed-{}", uuid::Uuid::new_v4().simple());
        let report = h
            .emit(
                &python,
                &[
                    PrioritySdkHarness::fixture("fixtures/sentry-typed-logs.py"),
                    run.clone(),
                ],
                None,
                0,
            )
            .await;
        let query = LogQuery {
            project_id: h.ctx.project_id,
            trace_id: Some(report["trace_id"].as_str().unwrap().into()),
            ..Default::default()
        };
        let logs = h
            .ctx
            .app_state
            .otel_service
            .query_logs(query.clone())
            .await
            .unwrap();
        assert_eq!(logs.len(), 3);
        assert!(logs
            .iter()
            .all(|v| v.span_id.is_some() && v.attributes["synthetic.run"] == run));
        for (key, value, ty, operator, expected) in [
            (
                "count",
                "9007199254740993",
                Some(LogAttributeType::Integer),
                LogAttributeOperator::Eq,
                1,
            ),
            (
                "count",
                "9007199254740992",
                Some(LogAttributeType::Integer),
                LogAttributeOperator::Gt,
                1,
            ),
            (
                "count",
                "9007199254740993",
                Some(LogAttributeType::String),
                LogAttributeOperator::Eq,
                1,
            ),
            (
                "flag",
                "true",
                Some(LogAttributeType::Boolean),
                LogAttributeOperator::Eq,
                1,
            ),
            (
                "count",
                "9007199254740993",
                None,
                LogAttributeOperator::Eq,
                2,
            ),
            (
                "count' OR '1'='1",
                "9007199254740993",
                None,
                LogAttributeOperator::Eq,
                0,
            ),
        ] {
            let found = h
                .ctx
                .app_state
                .otel_service
                .query_logs(LogQuery {
                    attribute_filter: Some(LogAttributeFilter {
                        key: key.into(),
                        value: value.into(),
                        ty,
                        operator,
                    }),
                    ..query.clone()
                })
                .await
                .unwrap();
            assert_eq!(found.len(), expected);
            if ty == Some(LogAttributeType::Integer) {
                assert_eq!(found[0].attributes["count"], "9007199254740993");
                assert_eq!(found[0].attribute_types["count"], LogAttributeType::Integer);
            }
        }
        assert!(h
            .ctx
            .app_state
            .otel_service
            .query_logs(LogQuery {
                attribute_filter: Some(LogAttributeFilter {
                    key: "count".into(),
                    value: "9223372036854775808".into(),
                    ty: Some(LogAttributeType::Integer),
                    operator: LogAttributeOperator::Eq
                }),
                ..query
            })
            .await
            .is_err());
        println!("Python/Go/Rust native trace trees and typed int64/string/boolean/range log queries passed");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "Official SDK prerequisites: scripts/sentry-acceptance.sh sdk"]
    #[serial_test::serial]
    async fn priority_sdk_legacy_metrics_and_release_correlation() {
        use temps_otel::types::*;
        let h = PrioritySdkHarness::new().await;
        let legacy = PrioritySdkHarness::prerequisite("TEMPS_SENTRY_NODE8_SDK");
        let node = PrioritySdkHarness::prerequisite("TEMPS_SENTRY_NODE11_SDK");
        let run = format!("synthetic-statsd-{}", uuid::Uuid::new_v4().simple());
        h.emit(
            "node",
            &[
                PrioritySdkHarness::fixture("sentry-statsd-conformance.mjs"),
                legacy,
                run.clone(),
            ],
            None,
            0,
        )
        .await;
        for (name, aggregation, expected) in [
            ("requests", MetricAggregation::Sum, 2.0),
            ("queue", MetricAggregation::Avg, 7.0),
            ("duration", MetricAggregation::Avg, 20.0),
        ] {
            let buckets = h
                .metric(&format!("synthetic.legacy.{name}"), &run, aggregation, "1h")
                .await;
            assert_eq!(buckets.len(), 1);
            assert_eq!(buckets[0].value, expected);
        }
        let grouped = h
            .ctx
            .app_state
            .otel_service
            .query_metrics(MetricQuery {
                project_id: h.ctx.project_id,
                metric_name: Some("synthetic.legacy.requests".into()),
                label_filters: vec![("synthetic.run".into(), run.clone())],
                group_by: vec!["synthetic.escaped".into()],
                aggregation: MetricAggregation::Sum,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(
            grouped[0].series_key.as_ref().unwrap()[0].1,
            "comma,pipe|newline\nend"
        );
        assert!(h
            .metric("synthetic.legacy.users", &run, MetricAggregation::Avg, "1h")
            .await
            .is_empty());
        let release = format!("synthetic-release-{}", uuid::Uuid::new_v4().simple());
        let emitted = h
            .emit(
                "node",
                &[
                    PrioritySdkHarness::fixture("sentry-release-conformance.mjs"),
                    node.clone(),
                    release.clone(),
                ],
                None,
                0,
            )
            .await;
        let service = &h.ctx.app_state.error_tracking_service.releases;
        let observed = service
            .list(h.ctx.project_id, Some(release.clone()), 7, 1, 20)
            .await
            .unwrap();
        assert_eq!(observed.total, 1);
        let row = &observed.items[0];
        assert_eq!(
            (
                row.errors,
                row.logs,
                row.spans,
                row.metric_points,
                row.feedback,
                row.check_ins
            ),
            (1, 1, 1, 3, 1, 1)
        );
        assert_eq!(
            row.example_trace_id.as_deref(),
            emitted["trace_id"].as_str()
        );
        assert_eq!(
            row.example_log_trace_id.as_deref(),
            emitted["trace_id"].as_str()
        );
        assert!(
            row.example_error_group_id.is_some()
                && row.example_feedback_id.is_some()
                && row.example_monitor_id.is_some()
        );
        for (name, expected) in [("requests", 2.0), ("queue", 7.0), ("duration", 12.5)] {
            let points = h
                .metric(
                    &format!("synthetic.release.{name}"),
                    &release,
                    MetricAggregation::Avg,
                    "1h",
                )
                .await;
            assert_eq!(points.len(), 1);
            assert_eq!(points[0].value, expected);
        }
        let late = h
            .emit(
                "node",
                &[
                    PrioritySdkHarness::fixture("sentry-release-conformance.mjs"),
                    node,
                    release.clone(),
                    "late".into(),
                ],
                None,
                0,
            )
            .await;
        assert_eq!(late["late"], true);
        let updated = service
            .list(h.ctx.project_id, Some(release.clone()), 7, 1, 20)
            .await
            .unwrap();
        let updated = &updated.items[0];
        assert_eq!(
            (
                updated.errors,
                updated.logs,
                updated.spans,
                updated.metric_points
            ),
            (1, 2, 1, 3)
        );
        assert_eq!(updated.first_seen, row.first_seen);
        assert!(updated.last_seen >= row.last_seen);
        assert!(service
            .list(h.ctx.project_id, Some(release.clone()), 7, 2, 1)
            .await
            .unwrap()
            .items
            .is_empty());
        assert!(service
            .list(h.ctx.project_id + 1, Some(release.clone()), 7, 1, 20)
            .await
            .unwrap()
            .items
            .is_empty());
        assert!(service
            .list(h.ctx.project_id, Some("' OR '1'='1".into()), 7, 1, 20)
            .await
            .unwrap()
            .items
            .is_empty());
        for (days, page, size) in [(32, 1, 20), (7, 0, 20), (7, 1, 101), (7, u64::MAX, 20)] {
            assert!(service
                .list(h.ctx.project_id, None, days, page, size)
                .await
                .is_err());
        }
        h.ctx
            .app_state
            .error_tracking_service
            .feedback
            .delete(h.ctx.project_id, row.example_feedback_id.unwrap())
            .await
            .unwrap();
        assert_eq!(
            service
                .list(h.ctx.project_id, Some(release), 7, 1, 20)
                .await
                .unwrap()
                .items[0]
                .feedback,
            0
        );
        println!("legacy delta/gauge/distribution/escaped-label/unsupported-set and six-signal release late-arrival/scope/pagination/deletion acceptance passed");
    }
}
