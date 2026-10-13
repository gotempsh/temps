// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! JSON body extractor for the public analytics ingest routes
//! (gotempsh/temps#1416).
//!
//! Identical to [`axum::Json`] except for one case: a **keyed** request may
//! also send its JSON as `Content-Type: text/plain`.
//!
//! `text/plain` is a CORS-safelisted content type, so a `navigator.sendBeacon`
//! with a `text/plain` `Blob` is sent without a preflight. That is the only way
//! a page-unload beacon reliably leaves the page cross-origin: beacons always
//! use credentials mode `include`, and a preflighted credentialed beacon is
//! dropped whenever the CORS answer is anything short of a credentialed grant.
//!
//! The body is parsed by exactly the same code as an `application/json` one —
//! [`axum::Json::from_bytes`] — under the same `DefaultBodyLimit`, so it gets
//! the same `400`/`413`/`422` answers and the same schema. Only the
//! content-type gate differs.
//!
//! # Why keyed requests only
//!
//! A `text/plain` POST is a "simple" request: any page on the internet can make
//! a browser send one, with that browser's cookies, without asking first. The
//! Host-resolved (unkeyed) path takes visitor identity from the Temps-issued
//! `_temps_visitor_id`/`_temps_sid` cookies, so accepting `text/plain` there
//! would let any third-party page write events into a visitor's real session.
//! Requiring `application/json` keeps every cross-origin write to that path
//! behind a preflight, which is answered without credentials, so no cookie
//! ever arrives with one.
//!
//! The keyed path has no such exposure: a keyed request from another origin
//! never takes identity from a cookie (see [`super::request::IngestBranch`]).

use axum::body::Bytes;
use axum::extract::rejection::JsonRejection;
use axum::extract::{FromRequest, Request};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::de::DeserializeOwned;
use temps_core::error_builder::ErrorBuilder;

use super::request::analytics_key;

/// [`axum::Json`], plus `text/plain` on keyed ingest requests. See the module
/// docs.
#[derive(Debug, Clone, Copy, Default)]
pub struct IngestJson<T>(pub T);

impl<T, S> FromRequest<S> for IngestJson<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        let keyed = analytics_key(request.headers(), request.uri().query()).is_some();

        if keyed && is_text_plain(request.headers()) {
            let bytes = Bytes::from_request(request, state)
                .await
                .map_err(IntoResponse::into_response)?;
            let Json(value) = Json::<T>::from_bytes(&bytes).map_err(IntoResponse::into_response)?;
            return Ok(Self(value));
        }

        match Json::<T>::from_request(request, state).await {
            Ok(Json(value)) => Ok(Self(value)),
            // The keyed path accepts a second type, so its 415 has to say so.
            // The unkeyed path keeps axum's own answer, byte for byte.
            Err(JsonRejection::MissingJsonContentType(_)) if keyed => {
                Err(unsupported_keyed_content_type())
            }
            Err(rejection) => Err(rejection.into_response()),
        }
    }
}

/// `text/plain`, with or without parameters (`;charset=UTF-8`), any case.
fn is_text_plain(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("text/plain"))
}

fn unsupported_keyed_content_type() -> Response {
    ErrorBuilder::new(StatusCode::UNSUPPORTED_MEDIA_TYPE)
        .title("Unsupported analytics payload content type")
        .detail(
            "Analytics ingest requests that carry an ingest key must send a JSON body with \
             Content-Type application/json or text/plain.",
        )
        .build()
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{to_bytes, Body};
    use axum::extract::DefaultBodyLimit;
    use axum::routing::post;
    use axum::Router;
    use serde::Deserialize;
    use tower::ServiceExt;

    #[derive(Debug, Deserialize)]
    struct Payload {
        event_name: String,
    }

    const KEYED_URI: &str = "/ingest?temps_key=pa_test";

    fn app() -> Router {
        Router::new()
            .route(
                "/ingest",
                post(|IngestJson(payload): IngestJson<Payload>| async move { payload.event_name }),
            )
            .layer(DefaultBodyLimit::max(64))
    }

    async fn send(uri: &str, content_type: Option<&str>, body: &str) -> (StatusCode, String) {
        let mut builder = axum::http::Request::builder().method("POST").uri(uri);
        if let Some(content_type) = content_type {
            builder = builder.header(header::CONTENT_TYPE, content_type);
        }
        let response = app()
            .oneshot(builder.body(Body::from(body.to_string())).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (status, String::from_utf8_lossy(&body).into_owned())
    }

    #[tokio::test]
    async fn keyed_text_plain_body_is_parsed_as_json() {
        let (status, body) = send(
            KEYED_URI,
            Some("text/plain;charset=UTF-8"),
            r#"{"event_name":"page_leave"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body, "page_leave");
    }

    #[tokio::test]
    async fn keyed_text_plain_is_matched_case_insensitively() {
        let (status, _) = send(KEYED_URI, Some("Text/Plain"), r#"{"event_name":"x"}"#).await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn header_key_also_unlocks_text_plain() {
        let response = app()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/ingest")
                    .header(header::CONTENT_TYPE, "text/plain")
                    .header(
                        super::super::request::ANALYTICS_INGEST_KEY_HEADER,
                        "pa_test",
                    )
                    .body(Body::from(r#"{"event_name":"x"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn keyed_application_json_still_works() {
        let (status, body) = send(
            KEYED_URI,
            Some("application/json"),
            r#"{"event_name":"page_view"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body, "page_view");
    }

    #[tokio::test]
    async fn unkeyed_text_plain_is_still_rejected_as_before() {
        let (status, body) = send("/ingest", Some("text/plain"), r#"{"event_name":"x"}"#).await;
        assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
        // axum's own wording: the unkeyed path is unchanged.
        assert!(body.contains("application/json"), "{body}");
        assert!(!body.contains("text/plain"), "{body}");
    }

    #[tokio::test]
    async fn keyed_unsupported_type_names_both_accepted_types() {
        let (status, body) = send(
            KEYED_URI,
            Some("application/x-www-form-urlencoded"),
            "event_name=x",
        )
        .await;
        assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
        assert!(body.contains("application/json"), "{body}");
        assert!(body.contains("text/plain"), "{body}");
    }

    #[tokio::test]
    async fn keyed_missing_content_type_is_a_415() {
        let (status, _) = send(KEYED_URI, None, r#"{"event_name":"x"}"#).await;
        assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }

    #[tokio::test]
    async fn keyed_text_plain_with_malformed_json_is_a_400() {
        let (status, _) = send(KEYED_URI, Some("text/plain"), "{not json").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn keyed_text_plain_with_the_wrong_shape_is_a_422() {
        let (status, _) = send(KEYED_URI, Some("text/plain"), r#"{"other":1}"#).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn keyed_text_plain_obeys_the_body_limit() {
        let oversized = format!(r#"{{"event_name":"{}"}}"#, "a".repeat(200));
        let (status, _) = send(KEYED_URI, Some("text/plain"), &oversized).await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);

        // Same limit, same answer, as the JSON path.
        let (status, _) = send(KEYED_URI, Some("application/json"), &oversized).await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    }
}
