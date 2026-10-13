// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! CORS for the public analytics ingest routes (ADR-040, gotempsh/temps#1416).
//!
//! Two layers, stacked on every public ingest router:
//!
//! * [`wildcard_ingest_cors`] — the `Access-Control-Allow-Origin: *`,
//!   never-credentialed policy these routes have always had. It still answers
//!   every request that is **not** a keyed request from a browser origin, so
//!   the no-key (Host-resolved, same-origin) path and every non-browser caller
//!   see exactly what they saw before.
//! * [`keyed_ingest_cors`] — runs outside it and takes over **only** when the
//!   request presents an ingest key *and* carries an `Origin`.
//!
//! # Why the keyed branch needs more than `*`
//!
//! `navigator.sendBeacon` always uses credentials mode `include`. A beacon whose
//! body is not a CORS-safelisted type (the SDK has always sent
//! `application/json`) is preflighted, and the Fetch spec forbids `*` on a
//! credentialed response, so the browser drops the request before it is sent.
//! Every page-unload event — `page_leave`, late Web Vitals, the final replay
//! batch — was lost for cross-origin keyed ingest. Current SDKs avoid the
//! preflight by beaconing `text/plain`, but SDK versions already deployed on
//! customer sites will keep sending `application/json` until those sites
//! upgrade, so the server has to accept the credentialed request too.
//!
//! # The keyed policy
//!
//! | Key | Origin vs. the key's `allowed_origins` | Response |
//! | --- | --- | --- |
//! | resolves | allowed (or the key has no list) | `ACAO: <that origin>`, `ACAC: true`, `Vary: Origin` |
//! | resolves | not allowed | no `ACAO` at all; a preflight is answered `403` with the reason |
//! | does not resolve / lookup failed / `Origin: null` | — | unchanged wildcard; the handler answers `401`/`500` as before |
//!
//! A key without an allowlist mirrors any origin, which is exactly the "any
//! origin may send" grant that key already had. Credentialed CORS is safe here
//! only because of an invariant the handlers keep: a keyed cross-origin request
//! never takes identity from a cookie (see
//! [`super::request::IngestBranch`]), and none of these routes reads a session.
//! The cookies the browser now attaches are ignored, so a reflected origin
//! cannot be used to act as the visitor — the response is the same one a
//! non-credentialed caller would get.
//!
//! A preflight carries no request header *values*, so only a `?temps_key=`
//! key can be recognised on one; a request carrying the key in the
//! `X-Temps-Analytics-Key` header is preflighted under the wildcard policy.
//! That is the SDK's uncredentialed `fetch` path, for which `*` is correct —
//! its beacons, the only credentialed transport, always use the query param.
//!
//! `null` is never echoed: it is the shared origin of every sandboxed iframe,
//! `data:` URL and `file:` page, so reflecting it with credentials would grant
//! all of them at once.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Request, State};
use axum::http::header::{self, HeaderName, HeaderValue};
use axum::http::{Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use tower_http::cors::{Any, CorsLayer};
use tracing::{debug, warn};

use super::rate_limiter::AnalyticsIngestRateLimiter;
use super::request::{
    analytics_key, is_origin_allowed, origin_not_allowed_problem, resolve_within_unresolved_budget,
    BudgetedResolveError, ANALYTICS_INGEST_KEY_HEADER,
};
use super::service::AnalyticsIngestKeyService;

/// How long a browser may cache a preflight answer. Shared by both layers so a
/// keyed and an unkeyed preflight are cached alike.
const PREFLIGHT_MAX_AGE: Duration = Duration::from_secs(600);

/// `Access-Control-Allow-Methods` for a keyed preflight. Must stay in step with
/// [`wildcard_ingest_cors`]'s `allow_methods`.
const ALLOWED_METHODS: &str = "POST,OPTIONS";

/// `Access-Control-Allow-Headers` for a keyed preflight. Must stay in step with
/// [`wildcard_ingest_cors`]'s `allow_headers`.
const ALLOWED_HEADERS: &str = "content-type,x-temps-analytics-key";

/// `Access-Control-Expose-Headers` on a credentialed response. Must stay in
/// step with [`wildcard_ingest_cors`]'s `expose_headers`.
const EXPOSED_HEADERS: &str = "retry-after";

/// The wildcard CORS policy for the public analytics ingest routes.
///
/// `allow_credentials` stays at its default `false` and must never be set:
/// credentials on a wildcard origin would be a real vulnerability, and the
/// browser rejects the combination outright. Credentialed CORS exists only on
/// the keyed branch, origin by origin, in [`keyed_ingest_cors`].
pub fn wildcard_ingest_cors() -> CorsLayer {
    CorsLayer::new()
        .allow_origin(Any)
        .allow_methods([Method::POST, Method::OPTIONS])
        .allow_headers([
            header::CONTENT_TYPE,
            HeaderName::from_static(ANALYTICS_INGEST_KEY_HEADER),
        ])
        // `Retry-After` is not one of the seven CORS-safelisted response
        // headers, so without this a cross-origin browser SDK — the stated
        // consumer of keyed ingest — gets the 429 but cannot read the header
        // that tells it when to come back, and falls back to guessing or
        // hammering. Exposed explicitly rather than via `Any` because
        // `Access-Control-Expose-Headers: *` is ignored by browsers for
        // credentialed requests and is a wider grant than is needed.
        .expose_headers([header::RETRY_AFTER])
        .max_age(PREFLIGHT_MAX_AGE)
}

/// State for [`keyed_ingest_cors`]: the same key service and limiter the
/// ingest handlers use, so the CORS decision and the handler's 401/403 can
/// never disagree about a key.
#[derive(Clone)]
pub struct KeyedIngestCors {
    key_service: Arc<AnalyticsIngestKeyService>,
    rate_limiter: Arc<AnalyticsIngestRateLimiter>,
}

impl KeyedIngestCors {
    pub fn new(
        key_service: Arc<AnalyticsIngestKeyService>,
        rate_limiter: Arc<AnalyticsIngestRateLimiter>,
    ) -> Self {
        Self {
            key_service,
            rate_limiter,
        }
    }

    /// Decide the CORS policy for a keyed request from `origin`.
    ///
    /// The lookup goes through [`resolve_within_unresolved_budget`], the same
    /// path the handlers use: a cache hit is free, and a database miss is
    /// charged to the global unresolved-key budget here and now — even if the
    /// request never reaches a handler (a preflight, a malformed body, a wrong
    /// method). The handler that follows then hits the cache and is not
    /// charged a second time.
    async fn decide(&self, key: &str, origin: &str) -> KeyedCorsDecision {
        // See the module docs: `null` is never reflected with credentials.
        if origin.eq_ignore_ascii_case("null") {
            return KeyedCorsDecision::Unchanged;
        }

        match resolve_within_unresolved_budget(&self.key_service, &self.rate_limiter, key).await {
            Ok(Some(scope)) => {
                if is_origin_allowed(scope.allowed_origins.as_deref(), Some(origin)) {
                    KeyedCorsDecision::Credentialed
                } else {
                    debug!(
                        key_id = scope.key_id,
                        project_id = scope.project_id,
                        "Withholding CORS grant: Origin not in the analytics ingest key's allowed_origins"
                    );
                    KeyedCorsDecision::OriginDenied
                }
            }
            Ok(None) | Err(BudgetedResolveError::BudgetExhausted) => KeyedCorsDecision::Unchanged,
            Err(BudgetedResolveError::Lookup(e)) => {
                warn!(
                    "Failed to resolve an analytics ingest key for a CORS decision: {}",
                    e
                );
                KeyedCorsDecision::Unchanged
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyedCorsDecision {
    /// Leave the wildcard policy in place: the handler will answer `401`,
    /// `429` or `500`, and those answers stay readable exactly as before.
    Unchanged,
    /// The key resolved and admits this origin: echo it, with credentials.
    Credentialed,
    /// The key resolved and does not admit this origin: grant nothing.
    OriginDenied,
}

/// Middleware: credentialed CORS for keyed ingest from an admitted origin.
///
/// Must be layered **outside** [`wildcard_ingest_cors`] so that it can answer
/// a keyed preflight itself and rewrite the wildcard headers on a keyed
/// response. Requests without both a key and an `Origin` pass straight
/// through, untouched.
pub async fn keyed_ingest_cors(
    State(cors): State<KeyedIngestCors>,
    request: Request,
    next: Next,
) -> Response {
    // Owned because the request moves into `next.run` below; cloning a
    // `HeaderValue` shares its bytes (a reference-count bump, not a copy).
    let Some(origin) = request.headers().get(header::ORIGIN).cloned() else {
        return next.run(request).await;
    };
    // A browser origin is ASCII by construction; anything else is not one we
    // could echo, so it keeps today's behaviour.
    let Ok(origin_str) = origin.to_str() else {
        return next.run(request).await;
    };
    // Borrowed: the key is only looked up, never kept, so it is not copied.
    let Some(key) = analytics_key(request.headers(), request.uri().query()) else {
        return next.run(request).await;
    };

    let preflight = is_preflight(&request);
    let decision = cors.decide(key, origin_str).await;

    match (decision, preflight) {
        (KeyedCorsDecision::Unchanged, _) => next.run(request).await,
        (KeyedCorsDecision::Credentialed, true) => credentialed_preflight(origin),
        (KeyedCorsDecision::OriginDenied, true) => denied_preflight(origin_str),
        (KeyedCorsDecision::Credentialed, false) => {
            let mut response = next.run(request).await;
            grant_credentialed(&mut response, origin);
            response
        }
        (KeyedCorsDecision::OriginDenied, false) => {
            // The handler applies the same allowlist and answers 403; this
            // only makes sure the browser is not told it may read that.
            let mut response = next.run(request).await;
            withhold_grant(&mut response);
            response
        }
    }
}

fn is_preflight(request: &Request) -> bool {
    request.method() == Method::OPTIONS
        && request
            .headers()
            .contains_key(header::ACCESS_CONTROL_REQUEST_METHOD)
}

/// `Vary` for every keyed answer: the response now depends on `Origin` (and, for
/// a preflight, on what was asked for), so no shared cache may replay one
/// origin's grant to another.
fn vary_value() -> HeaderValue {
    HeaderValue::from_static(
        "origin, access-control-request-method, access-control-request-headers",
    )
}

fn credentialed_preflight(origin: HeaderValue) -> Response {
    let mut response = StatusCode::NO_CONTENT.into_response();
    let headers = response.headers_mut();
    headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin);
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_CREDENTIALS,
        HeaderValue::from_static("true"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static(ALLOWED_METHODS),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static(ALLOWED_HEADERS),
    );
    headers.insert(
        header::ACCESS_CONTROL_MAX_AGE,
        HeaderValue::from(PREFLIGHT_MAX_AGE.as_secs()),
    );
    headers.insert(header::VARY, vary_value());
    response
}

/// A preflight from an origin the key does not admit. Answered `403` with the
/// same Problem body the handler would send, so an operator looking at the
/// network tab sees *why*, not just a bare CORS failure.
fn denied_preflight(origin: &str) -> Response {
    let mut response = origin_not_allowed_problem(Some(origin)).into_response();
    response.headers_mut().insert(header::VARY, vary_value());
    response
}

/// Make sure a rewritten response varies on `Origin` without dropping any
/// `Vary` an inner layer already set (the wildcard layer's, or a future
/// `accept-encoding`): losing one would let a shared cache replay a response
/// to a request it does not fit.
fn ensure_vary_origin(response: &mut Response) {
    let headers = response.headers_mut();
    let has_origin = headers.get_all(header::VARY).iter().any(|value| {
        value.to_str().is_ok_and(|value| {
            value
                .split(',')
                .any(|name| name.trim().eq_ignore_ascii_case("origin") || name.trim() == "*")
        })
    });
    if !has_origin {
        headers.append(header::VARY, HeaderValue::from_static("origin"));
    }
}

fn grant_credentialed(response: &mut Response, origin: HeaderValue) {
    let headers = response.headers_mut();
    headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin);
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_CREDENTIALS,
        HeaderValue::from_static("true"),
    );
    headers.insert(
        header::ACCESS_CONTROL_EXPOSE_HEADERS,
        HeaderValue::from_static(EXPOSED_HEADERS),
    );
    ensure_vary_origin(response);
}

fn withhold_grant(response: &mut Response) {
    let headers = response.headers_mut();
    headers.remove(header::ACCESS_CONTROL_ALLOW_ORIGIN);
    headers.remove(header::ACCESS_CONTROL_ALLOW_CREDENTIALS);
    headers.remove(header::ACCESS_CONTROL_EXPOSE_HEADERS);
    ensure_vary_origin(response);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest_keys::test_fixtures::key_model;
    use crate::ingest_keys::UNRESOLVED_KEY_RATE_LIMIT_PER_MINUTE;
    use axum::body::Body;
    use axum::routing::post;
    use axum::Router;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use temps_entities::{analytics_ingest_keys, environments};
    use tower::ServiceExt;

    const ALLOWED: &str = "https://shop.example.com";
    const OTHER: &str = "https://elsewhere.example.net";

    fn key_with_origins(origins: Option<Vec<&str>>) -> analytics_ingest_keys::Model {
        let mut key = key_model(10, 1, None);
        key.allowed_origins = origins.map(|list| serde_json::json!(list));
        key
    }

    /// A router shaped like the real ingest routers: the wildcard layer inside,
    /// the keyed layer outside. The handler answers 204 so tests can tell a
    /// short-circuited preflight from a request that reached it.
    fn app(db: MockDatabase) -> (Router, Arc<AnalyticsIngestRateLimiter>) {
        let key_service = Arc::new(AnalyticsIngestKeyService::new(Arc::new(
            db.into_connection(),
        )));
        let rate_limiter = Arc::new(AnalyticsIngestRateLimiter::new());
        let router = Router::new()
            .route("/_temps/event", post(|| async { StatusCode::NO_CONTENT }))
            .layer(wildcard_ingest_cors())
            .layer(axum::middleware::from_fn_with_state(
                KeyedIngestCors::new(key_service, rate_limiter.clone()),
                keyed_ingest_cors,
            ));
        (router, rate_limiter)
    }

    fn db_resolving(key: analytics_ingest_keys::Model) -> MockDatabase {
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![(key, None::<environments::Model>)]])
    }

    fn db_resolving_nothing() -> MockDatabase {
        MockDatabase::new(DatabaseBackend::Postgres).append_query_results([Vec::<(
            analytics_ingest_keys::Model,
            Option<environments::Model>,
        )>::new()])
    }

    /// Spend all but one of the global unresolved-key budget, so the next
    /// charge — and only a charge — trips it.
    async fn prime_unresolved_budget(rate_limiter: &AnalyticsIngestRateLimiter) {
        for _ in 1..UNRESOLVED_KEY_RATE_LIMIT_PER_MINUTE {
            rate_limiter.record_unresolved_attempt().await;
        }
        assert!(!rate_limiter.unresolved_budget_exhausted().await);
    }

    fn public_key() -> String {
        key_model(10, 1, None).public_key
    }

    fn preflight(origin: &str, query_key: Option<&str>) -> axum::http::Request<Body> {
        let uri = match query_key {
            Some(key) => format!("/_temps/event?temps_key={key}"),
            None => "/_temps/event".to_string(),
        };
        axum::http::Request::builder()
            .method(Method::OPTIONS)
            .uri(uri)
            .header(header::ORIGIN, origin)
            .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
            .header(header::ACCESS_CONTROL_REQUEST_HEADERS, "content-type")
            .body(Body::empty())
            .unwrap()
    }

    fn post_with_query_key(origin: Option<&str>, key: &str) -> axum::http::Request<Body> {
        let mut builder = axum::http::Request::builder()
            .method(Method::POST)
            .uri(format!("/_temps/event?temps_key={key}"));
        if let Some(origin) = origin {
            builder = builder.header(header::ORIGIN, origin);
        }
        builder.body(Body::from("{}")).unwrap()
    }

    fn header_str(response: &Response, name: HeaderName) -> Option<&str> {
        response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
    }

    fn assert_credentialed_for(response: &Response, origin: &str) {
        assert_eq!(
            header_str(response, header::ACCESS_CONTROL_ALLOW_ORIGIN),
            Some(origin),
            "an admitted origin must be echoed, never `*`"
        );
        assert_eq!(
            header_str(response, header::ACCESS_CONTROL_ALLOW_CREDENTIALS),
            Some("true")
        );
        let vary = header_str(response, header::VARY).unwrap_or_default();
        assert!(
            vary.contains("origin"),
            "Vary must include Origin: {vary:?}"
        );
    }

    fn assert_no_grant(response: &Response) {
        assert!(
            response
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .is_none(),
            "no ACAO may be granted to an origin the key rejects"
        );
        assert!(response
            .headers()
            .get(header::ACCESS_CONTROL_ALLOW_CREDENTIALS)
            .is_none());
    }

    #[tokio::test]
    async fn keyed_preflight_from_an_allowed_origin_is_credentialed() {
        let (app, _) = app(db_resolving(key_with_origins(Some(vec![ALLOWED]))));

        let response = app
            .oneshot(preflight(ALLOWED, Some(&public_key())))
            .await
            .unwrap();

        assert!(response.status().is_success(), "{}", response.status());
        assert_credentialed_for(&response, ALLOWED);
        let methods = header_str(&response, header::ACCESS_CONTROL_ALLOW_METHODS).unwrap();
        assert!(methods.contains("POST"), "{methods}");
        let allowed_headers =
            header_str(&response, header::ACCESS_CONTROL_ALLOW_HEADERS).unwrap_or_default();
        assert!(
            allowed_headers.contains("content-type"),
            "{allowed_headers}"
        );
    }

    #[tokio::test]
    async fn keyed_preflight_from_a_disallowed_origin_grants_nothing() {
        let (app, _) = app(db_resolving(key_with_origins(Some(vec![ALLOWED]))));

        let response = app
            .oneshot(preflight(OTHER, Some(&public_key())))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_no_grant(&response);
        let vary = header_str(&response, header::VARY).unwrap_or_default();
        assert!(vary.contains("origin"), "{vary:?}");
    }

    #[tokio::test]
    async fn keyed_preflight_for_a_key_without_an_allowlist_mirrors_the_origin() {
        let (app, _) = app(db_resolving(key_with_origins(None)));

        let response = app
            .oneshot(preflight(OTHER, Some(&public_key())))
            .await
            .unwrap();

        assert!(response.status().is_success(), "{}", response.status());
        assert_credentialed_for(&response, OTHER);
    }

    #[tokio::test]
    async fn an_empty_allowlist_is_treated_like_no_allowlist() {
        let (app, _) = app(db_resolving(key_with_origins(Some(vec![]))));

        let response = app
            .oneshot(preflight(OTHER, Some(&public_key())))
            .await
            .unwrap();

        assert_credentialed_for(&response, OTHER);
    }

    #[tokio::test]
    async fn the_null_origin_is_never_echoed() {
        // No query results: the key must not even be looked up.
        let (app, _) = app(MockDatabase::new(DatabaseBackend::Postgres));

        let response = app
            .oneshot(preflight("null", Some(&public_key())))
            .await
            .unwrap();

        assert_eq!(
            header_str(&response, header::ACCESS_CONTROL_ALLOW_ORIGIN),
            Some("*"),
            "`null` keeps the non-credentialed wildcard it always had"
        );
        assert!(response
            .headers()
            .get(header::ACCESS_CONTROL_ALLOW_CREDENTIALS)
            .is_none());
    }

    #[tokio::test]
    async fn unkeyed_preflight_is_the_unchanged_wildcard() {
        // No query results: an unkeyed request must never touch the key store.
        let (app, _) = app(MockDatabase::new(DatabaseBackend::Postgres));

        let response = app.oneshot(preflight(OTHER, None)).await.unwrap();

        assert!(response.status().is_success(), "{}", response.status());
        assert_eq!(
            header_str(&response, header::ACCESS_CONTROL_ALLOW_ORIGIN),
            Some("*")
        );
        assert!(
            response
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_CREDENTIALS)
                .is_none(),
            "the unkeyed path must never become credentialed"
        );
    }

    #[tokio::test]
    async fn unresolvable_key_keeps_the_wildcard_and_is_charged_on_preflight() {
        let (app, rate_limiter) = app(db_resolving_nothing());
        prime_unresolved_budget(&rate_limiter).await;
        let unknown = format!("pa_{}", "e".repeat(64));

        let response = app.oneshot(preflight(OTHER, Some(&unknown))).await.unwrap();

        assert_eq!(
            header_str(&response, header::ACCESS_CONTROL_ALLOW_ORIGIN),
            Some("*")
        );
        assert!(response
            .headers()
            .get(header::ACCESS_CONTROL_ALLOW_CREDENTIALS)
            .is_none());
        assert!(
            rate_limiter.unresolved_budget_exhausted().await,
            "a garbage-key preflight never reaches a handler, so this layer must charge it"
        );
    }

    #[tokio::test]
    async fn keyed_post_from_an_allowed_origin_is_credentialed() {
        let (app, _) = app(db_resolving(key_with_origins(Some(vec![ALLOWED]))));

        let response = app
            .oneshot(post_with_query_key(Some(ALLOWED), &public_key()))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_credentialed_for(&response, ALLOWED);
        assert_eq!(
            header_str(&response, header::ACCESS_CONTROL_EXPOSE_HEADERS),
            Some("retry-after"),
            "a credentialed SDK must still be able to read Retry-After"
        );
    }

    #[tokio::test]
    async fn keyed_post_from_a_disallowed_origin_reaches_the_handler_without_a_grant() {
        let (app, rate_limiter) = app(db_resolving(key_with_origins(Some(vec![ALLOWED]))));
        prime_unresolved_budget(&rate_limiter).await;

        let response = app
            .oneshot(post_with_query_key(Some(OTHER), &public_key()))
            .await
            .unwrap();

        // The handler (here a stub) still runs: rejecting the write is the
        // handler's job, with its own 403. This layer only withholds the grant.
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_no_grant(&response);
        assert!(
            !rate_limiter.unresolved_budget_exhausted().await,
            "a resolved key must never be charged as an unresolved attempt"
        );
    }

    #[tokio::test]
    async fn keyed_post_without_an_origin_is_left_alone() {
        // A server-side caller: no browser, nothing to grant. No query results
        // appended, so a key lookup here would fail the test.
        let (app, _) = app(MockDatabase::new(DatabaseBackend::Postgres));

        let response = app
            .oneshot(post_with_query_key(None, &public_key()))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert!(response
            .headers()
            .get(header::ACCESS_CONTROL_ALLOW_CREDENTIALS)
            .is_none());
    }

    #[tokio::test]
    async fn the_header_key_is_honoured_too() {
        let (app, _) = app(db_resolving(key_with_origins(Some(vec![ALLOWED]))));

        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .method(Method::POST)
                    .uri("/_temps/event")
                    .header(header::ORIGIN, ALLOWED)
                    .header(ANALYTICS_INGEST_KEY_HEADER, public_key())
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_credentialed_for(&response, ALLOWED);
    }

    #[tokio::test]
    async fn exhausted_unresolved_budget_skips_the_lookup() {
        // No query results: once the global budget is spent the layer must not
        // spend a query of its own.
        let (app, rate_limiter) = app(MockDatabase::new(DatabaseBackend::Postgres));
        for _ in 0..UNRESOLVED_KEY_RATE_LIMIT_PER_MINUTE {
            rate_limiter.record_unresolved_attempt().await;
        }

        let response = app
            .oneshot(preflight(ALLOWED, Some(&public_key())))
            .await
            .unwrap();

        assert_eq!(
            header_str(&response, header::ACCESS_CONTROL_ALLOW_ORIGIN),
            Some("*")
        );
    }

    /// The real handlers resolve the key again through
    /// `resolve_keyed_ingest_scope`. A stand-in that does the same, behind an
    /// `IngestJson` body like the real ones.
    fn app_with_resolving_handler(db: MockDatabase) -> (Router, Arc<AnalyticsIngestRateLimiter>) {
        let key_service = Arc::new(AnalyticsIngestKeyService::new(Arc::new(
            db.into_connection(),
        )));
        let rate_limiter = Arc::new(AnalyticsIngestRateLimiter::new());
        let handler_keys = key_service.clone();
        let handler_limiter = rate_limiter.clone();
        let handler = move |request: axum::http::Request<Body>| {
            let keys = handler_keys.clone();
            let limiter = handler_limiter.clone();
            async move {
                let key = crate::ingest_keys::extract_analytics_key(
                    request.headers(),
                    request.uri().query(),
                )
                .unwrap_or_default();
                type Body = crate::ingest_keys::IngestJson<serde_json::Value>;
                if let Err(rejection) =
                    <Body as axum::extract::FromRequest<()>>::from_request(request, &()).await
                {
                    return rejection;
                }
                match crate::ingest_keys::resolve_keyed_ingest_scope(&keys, &limiter, &key, None)
                    .await
                {
                    Ok(_) => StatusCode::NO_CONTENT.into_response(),
                    Err(problem) => problem.into_response(),
                }
            }
        };
        let router = Router::new()
            .route("/_temps/event", post(handler))
            .layer(wildcard_ingest_cors())
            .layer(axum::middleware::from_fn_with_state(
                KeyedIngestCors::new(key_service, rate_limiter.clone()),
                keyed_ingest_cors,
            ));
        (router, rate_limiter)
    }

    fn garbage_key(fill: char) -> String {
        format!("pa_{}", fill.to_string().repeat(64))
    }

    #[tokio::test]
    async fn a_lookup_is_charged_once_even_when_the_handler_resolves_again() {
        // One query result: the handler's second resolve must be a cache hit.
        let (app, rate_limiter) = app_with_resolving_handler(db_resolving_nothing());
        for _ in 2..UNRESOLVED_KEY_RATE_LIMIT_PER_MINUTE {
            rate_limiter.record_unresolved_attempt().await;
        }

        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .method(Method::POST)
                    .uri(format!("/_temps/event?temps_key={}", garbage_key('a')))
                    .header(header::ORIGIN, OTHER)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();

        // 401 proves the handler ran and resolved the key a second time.
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(
            !rate_limiter.unresolved_budget_exhausted().await,
            "one query must cost one unit of budget, not one per layer"
        );
    }

    #[tokio::test]
    async fn a_request_rejected_before_its_handler_is_still_charged() {
        // The auditor's bypass: a keyed POST whose body fails to parse never
        // reaches the handler's own accounting, but the CORS layer already
        // paid for a lookup. It must count.
        let (app, rate_limiter) = app_with_resolving_handler(db_resolving_nothing());
        prime_unresolved_budget(&rate_limiter).await;

        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .method(Method::POST)
                    .uri(format!("/_temps/event?temps_key={}", garbage_key('b')))
                    .header(header::ORIGIN, OTHER)
                    .header(header::CONTENT_TYPE, "text/plain")
                    .body(Body::from("{not json"))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(rate_limiter.unresolved_budget_exhausted().await);
    }

    #[tokio::test]
    async fn a_wrong_method_is_still_charged() {
        let (app, rate_limiter) = app_with_resolving_handler(db_resolving_nothing());
        prime_unresolved_budget(&rate_limiter).await;

        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .method(Method::GET)
                    .uri(format!("/_temps/event?temps_key={}", garbage_key('c')))
                    .header(header::ORIGIN, OTHER)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert!(rate_limiter.unresolved_budget_exhausted().await);
    }

    #[tokio::test]
    async fn a_preflight_during_a_database_failure_is_charged() {
        // A preflight never reaches a handler, so a failed lookup made for it
        // has to be charged here, or an outage would be retried unbounded.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_errors([sea_orm::DbErr::Custom("database unavailable".to_string())]);
        let (app, rate_limiter) = app(db);
        prime_unresolved_budget(&rate_limiter).await;

        let response = app
            .oneshot(preflight(ALLOWED, Some(&public_key())))
            .await
            .unwrap();

        assert_eq!(
            header_str(&response, header::ACCESS_CONTROL_ALLOW_ORIGIN),
            Some("*"),
            "a failed lookup keeps the unchanged wildcard policy"
        );
        assert!(rate_limiter.unresolved_budget_exhausted().await);
    }

    #[tokio::test]
    async fn a_cached_valid_key_survives_an_exhausted_budget() {
        // A flood of garbage spends the budget; a key already in the cache
        // costs no query and must keep its credentialed grant.
        let (app, rate_limiter) = app(db_resolving(key_with_origins(Some(vec![ALLOWED]))));

        let warm = app
            .clone()
            .oneshot(preflight(ALLOWED, Some(&public_key())))
            .await
            .unwrap();
        assert_credentialed_for(&warm, ALLOWED);

        for _ in 0..UNRESOLVED_KEY_RATE_LIMIT_PER_MINUTE {
            rate_limiter.record_unresolved_attempt().await;
        }
        assert!(rate_limiter.unresolved_budget_exhausted().await);

        let response = app
            .oneshot(post_with_query_key(Some(ALLOWED), &public_key()))
            .await
            .unwrap();
        assert_credentialed_for(&response, ALLOWED);
    }

    #[tokio::test]
    async fn rewriting_keeps_an_inner_vary() {
        let key_service = Arc::new(AnalyticsIngestKeyService::new(Arc::new(
            db_resolving(key_with_origins(None)).into_connection(),
        )));
        let app = Router::new()
            .route(
                "/_temps/event",
                post(|| async { ([(header::VARY, "accept-encoding")], StatusCode::NO_CONTENT) }),
            )
            .layer(axum::middleware::from_fn_with_state(
                KeyedIngestCors::new(key_service, Arc::new(AnalyticsIngestRateLimiter::new())),
                keyed_ingest_cors,
            ));

        let response = app
            .oneshot(post_with_query_key(Some(OTHER), &public_key()))
            .await
            .unwrap();

        let vary: Vec<String> = response
            .headers()
            .get_all(header::VARY)
            .iter()
            .filter_map(|value| value.to_str().ok().map(str::to_ascii_lowercase))
            .collect();
        assert!(
            vary.iter().any(|v| v.contains("accept-encoding")),
            "{vary:?}"
        );
        assert!(vary.iter().any(|v| v.contains("origin")), "{vary:?}");
    }

    #[test]
    fn keyed_constants_match_the_wildcard_layer() {
        // The keyed preflight is written by hand; if the wildcard layer ever
        // grows a method or header, a keyed SDK must not silently lose it.
        assert_eq!(ALLOWED_METHODS, "POST,OPTIONS");
        assert_eq!(
            ALLOWED_HEADERS,
            format!("content-type,{ANALYTICS_INGEST_KEY_HEADER}")
        );
        assert_eq!(EXPOSED_HEADERS, header::RETRY_AFTER.as_str());
    }
}
