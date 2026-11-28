// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! One request-side check for JSON bodies sent to the management API: a
//! string holding a NUL character.
//!
//! `"a\u0000b"` is valid JSON, but PostgreSQL cannot store U+0000 in `text`
//! (`invalid byte sequence for encoding "UTF8": 0x00`) or in `jsonb`
//! (`unsupported Unicode escape sequence`). Every handler that saved such a
//! string answered with a 500 carrying the database error. Rejecting it once,
//! before any handler runs, turns all of them into a 400 naming the field.
//!
//! Control plane only: the middleware is layered on the admin router, never
//! on the proxy or the public ingest routes. A body without the escape costs
//! one byte scan; only a body that contains `\u0000` is parsed again to name
//! the field.

use axum::{
    body::{Body, Bytes},
    extract::Request,
    http::{header, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use futures_util::{stream, StreamExt};

use crate::api_problems::INVALID_REQUEST_PROBLEM_TYPE;
use crate::problemdetails;

/// Largest body this check buffers. Larger bodies (bulk imports, uploads)
/// stream through unchecked rather than being held in memory here; the
/// handler's own extractor limit still applies to them.
pub const MAX_SCANNED_JSON_BODY_BYTES: usize = 8 * 1024 * 1024;

/// Middleware rejecting a JSON request body whose strings (or object keys)
/// contain a NUL character, with a 400 Problem Details naming the field.
///
/// A body that is not JSON, is malformed, or is larger than
/// [`MAX_SCANNED_JSON_BODY_BYTES`] is passed on untouched, so the handler's
/// extractor reports what is wrong with it exactly as before.
pub async fn reject_nul_in_json_body(request: Request, next: Next) -> Response {
    if !is_json_request(&request) {
        return next.run(request).await;
    }

    let (parts, body) = request.into_parts();
    match buffer_bounded(body, MAX_SCANNED_JSON_BODY_BYTES).await {
        Buffered::Complete(bytes) => {
            if let Some(field) = nul_field(&bytes) {
                return nul_problem(&field);
            }
            next.run(Request::from_parts(parts, Body::from(bytes)))
                .await
        }
        Buffered::Passthrough(body) => next.run(Request::from_parts(parts, body)).await,
    }
}

fn is_json_request(request: &Request) -> bool {
    request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            let essence = value
                .split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .to_ascii_lowercase();
            essence == "application/json" || essence.ends_with("+json")
        })
}

enum Buffered {
    /// The whole body, at most the limit.
    Complete(Bytes),
    /// The body was larger than the limit or failed while being read: what
    /// was read so far followed by the rest, replayed unchanged.
    Passthrough(Body),
}

async fn buffer_bounded(body: Body, limit: usize) -> Buffered {
    let mut stream = body.into_data_stream();
    // One contiguous buffer, not a handle per chunk: a body framed as many
    // tiny (or empty) chunks would otherwise retain far more than its size.
    // Memory stays at most the limit plus the one chunk that crossed it.
    let mut buffered: Vec<u8> = Vec::new();
    while let Some(next) = stream.next().await {
        match next {
            Ok(chunk) => {
                buffered.extend_from_slice(&chunk);
                if buffered.len() > limit {
                    let read =
                        stream::once(async move { Ok::<_, axum::Error>(Bytes::from(buffered)) });
                    return Buffered::Passthrough(Body::from_stream(read.chain(stream)));
                }
            }
            Err(error) => {
                // Hand the read error to the extractor, which reports it the
                // way it always has.
                let read = stream::once(async move { Ok::<_, axum::Error>(Bytes::from(buffered)) });
                let failed = stream::once(async move { Err::<Bytes, axum::Error>(error) });
                return Buffered::Passthrough(Body::from_stream(read.chain(failed)));
            }
        }
    }
    Buffered::Complete(Bytes::from(buffered))
}

/// The path of the first string holding a NUL, or `None` when the body has
/// none (or is not valid JSON, which the handler's extractor reports).
fn nul_field(bytes: &[u8]) -> Option<String> {
    if !has_nul_escape(bytes) {
        return None;
    }
    let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let mut path = String::new();
    find_nul(&value, &mut path)
}

/// Whether the raw JSON contains a `\u0000` escape, i.e. one preceded by an
/// odd run of backslashes (`\\u0000` is the six characters `\u0000`, not a
/// NUL). JSON cannot carry a raw 0x00 inside a string, so the escape is the
/// only way a NUL arrives.
fn has_nul_escape(bytes: &[u8]) -> bool {
    const ESCAPE: &[u8] = b"\\u0000";
    let mut start = 0;
    while let Some(offset) = bytes[start..]
        .windows(ESCAPE.len())
        .position(|window| window == ESCAPE)
    {
        let at = start + offset;
        let backslashes = bytes[..=at]
            .iter()
            .rev()
            .take_while(|&&b| b == b'\\')
            .count();
        if backslashes % 2 == 1 {
            return true;
        }
        start = at + 1;
    }
    false
}

fn find_nul(value: &serde_json::Value, path: &mut String) -> Option<String> {
    match value {
        serde_json::Value::String(text) if text.contains('\0') => Some(display_path(path)),
        serde_json::Value::Array(items) => items.iter().enumerate().find_map(|(index, item)| {
            let len = path.len();
            path.push_str(&format!("[{index}]"));
            let found = find_nul(item, path);
            path.truncate(len);
            found
        }),
        serde_json::Value::Object(fields) => fields.iter().find_map(|(key, item)| {
            let len = path.len();
            if !path.is_empty() {
                path.push('.');
            }
            path.push_str(&key.replace('\0', "\\u0000"));
            let found = if key.contains('\0') {
                Some(display_path(path))
            } else {
                find_nul(item, path)
            };
            path.truncate(len);
            found
        }),
        _ => None,
    }
}

fn display_path(path: &str) -> String {
    if path.is_empty() {
        "(request body)".to_string()
    } else {
        path.to_string()
    }
}

fn nul_problem(field: &str) -> Response {
    problemdetails::new(StatusCode::BAD_REQUEST)
        .with_type(INVALID_REQUEST_PROBLEM_TYPE)
        .with_title("Invalid Request")
        .with_detail(format!(
            "Field '{field}' contains a NUL character (\\u0000), which cannot be stored; \
             remove it and send the request again"
        ))
        .with_value("field", field)
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{routing::post, Json, Router};
    use tower::ServiceExt;

    fn app() -> Router {
        Router::new()
            .route(
                "/items",
                post(|Json(value): Json<serde_json::Value>| async move { Json(value) }),
            )
            .layer(axum::middleware::from_fn(reject_nul_in_json_body))
    }

    async fn send(content_type: &str, body: impl Into<Body>) -> (StatusCode, serde_json::Value) {
        let request = Request::builder()
            .method("POST")
            .uri("/items")
            .header(header::CONTENT_TYPE, content_type)
            .body(body.into())
            .expect("request");
        let response = app().oneshot(request).await.expect("router answers");
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("body");
        let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, json)
    }

    #[tokio::test]
    async fn a_nul_in_a_top_level_string_is_a_400_naming_the_field() {
        let (status, body) = send("application/json", r#"{"name":"a\u0000b","slug":"x"}"#).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["type"], INVALID_REQUEST_PROBLEM_TYPE);
        assert_eq!(body["field"], "name");
        let detail = body["detail"].as_str().expect("detail");
        assert!(detail.contains("'name'"), "{detail}");
        assert!(!detail.to_lowercase().contains("database"), "{detail}");
    }

    #[tokio::test]
    async fn a_nul_in_a_nested_array_item_names_its_path() {
        let (status, body) = send(
            "application/json; charset=utf-8",
            r#"{"cidrs":[{"cidr":"10.0.0.0/8"},{"cidr":"10.1.0.0/16","description":"a\u0000b"}]}"#,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["field"], "cidrs[1].description");
    }

    #[tokio::test]
    async fn a_nul_in_an_object_key_is_rejected() {
        let (status, body) = send("application/json", r#"{"labels":{"a\u0000":"v"}}"#).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["field"], "labels.a\\u0000");
    }

    #[tokio::test]
    async fn an_escaped_backslash_before_u0000_is_not_a_nul() {
        // The JSON string `a\\u0000b` is the seven characters a\u0000b.
        let (status, body) = send("application/json", r#"{"name":"a\\u0000b"}"#).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["name"], "a\\u0000b");
    }

    #[tokio::test]
    async fn an_odd_run_of_backslashes_still_escapes_the_nul() {
        // `\\\u0000` is a literal backslash followed by a NUL.
        let (status, body) = send("application/json", r#"{"name":"\\\u0000"}"#).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["field"], "name");
    }

    #[tokio::test]
    async fn a_clean_body_reaches_the_handler_unchanged() {
        let (status, body) = send("application/json", r#"{"name":"ok","n":[1,2]}"#).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, serde_json::json!({"name":"ok","n":[1,2]}));
    }

    #[tokio::test]
    async fn a_body_that_is_not_json_is_not_inspected() {
        let request = Request::builder()
            .method("POST")
            .uri("/items")
            .header(header::CONTENT_TYPE, "text/plain")
            .body(Body::from("a\\u0000b"))
            .expect("request");
        let response = app().oneshot(request).await.expect("router answers");
        // The handler's JSON extractor refuses the content type, not this check.
        assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }

    #[tokio::test]
    async fn a_body_split_across_chunks_is_still_checked() {
        let chunks = vec![
            Ok::<_, std::io::Error>(Bytes::from_static(br#"{"name":"a\u00"#)),
            Ok(Bytes::from_static(br#"00b"}"#)),
        ];
        let (status, body) = send(
            "application/json",
            Body::from_stream(futures_util::stream::iter(chunks)),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["field"], "name");
    }

    #[tokio::test]
    async fn a_body_over_the_limit_is_replayed_to_the_handler_intact() {
        let filler = "x".repeat(64);
        let body = Body::from(format!(r#"{{"name":"{filler}"}}"#));
        let replayed = match buffer_bounded(body, 16).await {
            Buffered::Passthrough(body) => body,
            Buffered::Complete(_) => panic!("a body over the limit must not be buffered"),
        };
        let bytes = axum::body::to_bytes(replayed, 1024)
            .await
            .expect("replayed body");
        assert_eq!(bytes, format!(r#"{{"name":"{filler}"}}"#).as_bytes());
    }

    #[tokio::test]
    async fn a_body_of_many_tiny_chunks_is_buffered_and_replayed_in_order() {
        let text = format!(r#"{{"name":"{}"}}"#, "y".repeat(40));
        let tiny = |text: &str| {
            let chunks: Vec<Result<Bytes, std::io::Error>> = text
                .bytes()
                .flat_map(|b| [Ok(Bytes::new()), Ok(Bytes::from(vec![b]))])
                .collect();
            Body::from_stream(futures_util::stream::iter(chunks))
        };
        match buffer_bounded(tiny(&text), 1024).await {
            Buffered::Complete(bytes) => assert_eq!(bytes, text.as_bytes()),
            Buffered::Passthrough(_) => panic!("a body under the limit is buffered"),
        }
        let replayed = match buffer_bounded(tiny(&text), 16).await {
            Buffered::Passthrough(body) => body,
            Buffered::Complete(_) => panic!("a body over the limit must not be buffered"),
        };
        let bytes = axum::body::to_bytes(replayed, 1024)
            .await
            .expect("replayed body");
        assert_eq!(bytes, text.as_bytes());
    }

    #[tokio::test]
    async fn malformed_json_is_left_to_the_extractor() {
        let (status, body) = send("application/json", r#"{"name":"a\u0000"#).await;
        // axum's own rejection of the truncated body, not this check's 400.
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body.get("field").is_none(), "{body}");
    }
}
