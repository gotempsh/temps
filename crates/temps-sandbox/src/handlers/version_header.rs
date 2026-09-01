// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Middleware that stamps every `/v1/sandbox/*` response with the
//! `X-Sandbox-API-Version` header. Used for support triage — see
//! [ADR-009] for the full policy.
//!
//! Contract: the header value is diagnostic, not part of the API
//! contract. Clients must rely on the URL segment (`/v1/`) for
//! versioning, not this header. The header format may change between
//! releases without warning.
//!
//! Implementation note: we bind to `CARGO_PKG_VERSION` of this crate
//! (which tracks the workspace version) rather than the runtime
//! `TEMPS_VERSION` env var, because library crates don't see the build
//! script env vars from `temps-cli`. In the common case they're the
//! same value.
//!
//! [ADR-009]: ../../../docs/adr/009-sandbox-api-versioning.md

use axum::{
    http::{header::HeaderValue, HeaderName},
    middleware::Next,
    response::Response,
};

/// The response header name. Prefixed `X-` because it's a Temps-private,
/// non-standardized header (no IANA registration intent).
pub const VERSION_HEADER: HeaderName = HeaderName::from_static("x-sandbox-api-version");

/// The version value stamped into every response. Computed once at
/// compile time from this crate's `Cargo.toml`.
pub const VERSION_VALUE: &str = env!("CARGO_PKG_VERSION");

/// Axum middleware that appends `X-Sandbox-API-Version` to every
/// response passing through it.
///
/// Safe to apply to both success and error paths — we set the header
/// after the inner handler runs, so 2xx, 4xx, and 5xx responses all
/// carry it.
pub async fn inject_version_header(request: axum::extract::Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    // `HeaderValue::from_static` is infallible for ASCII, which
    // `CARGO_PKG_VERSION` always is. If someone puts unicode in a
    // Cargo version string they have bigger problems than this header.
    if let Ok(value) = HeaderValue::from_str(VERSION_VALUE) {
        response.headers_mut().insert(VERSION_HEADER, value);
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
        middleware,
        routing::get,
        Router,
    };
    use tower::ServiceExt;

    async fn hello() -> &'static str {
        "ok"
    }

    fn test_router() -> Router {
        Router::new()
            .route("/hello", get(hello))
            .layer(middleware::from_fn(inject_version_header))
    }

    #[tokio::test]
    async fn version_header_is_present_on_success() {
        let app = test_router();
        let req = Request::builder()
            .uri("/hello")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let hdr = response
            .headers()
            .get(&VERSION_HEADER)
            .expect("version header missing");
        // Format is loose — but must be non-empty.
        assert!(!hdr.to_str().unwrap().is_empty());
    }

    #[tokio::test]
    async fn version_header_value_matches_crate_version() {
        let app = test_router();
        let req = Request::builder()
            .uri("/hello")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        let hdr = response
            .headers()
            .get(&VERSION_HEADER)
            .unwrap()
            .to_str()
            .unwrap();
        assert_eq!(hdr, env!("CARGO_PKG_VERSION"));
    }

    #[tokio::test]
    async fn version_header_is_present_on_error_responses() {
        async fn broken() -> (StatusCode, &'static str) {
            (StatusCode::INTERNAL_SERVER_ERROR, "boom")
        }
        let app = Router::new()
            .route("/broken", get(broken))
            .layer(middleware::from_fn(inject_version_header));
        let req = Request::builder()
            .uri("/broken")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        // Contract: 5xx must still carry the header so support can see
        // which build produced the error.
        assert!(response.headers().contains_key(&VERSION_HEADER));
    }
}
