// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Problem Details for requests no handler answered itself: a path no route
//! matches, a method a route does not serve, and a body, query or path the
//! extractors rejected before the handler ran.
//!
//! Every other API error is `application/problem+json`. These three used to
//! be an empty 404/405 or a `text/plain` sentence, so a client could not tell
//! "this server version has no such endpoint" from "this object does not
//! exist", and had a second error format to parse.

use axum::body::HttpBody as _;
use axum::{
    body::Body,
    extract::Request,
    http::{header, HeaderValue, Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};

use crate::problemdetails::{self, Problem};

/// `type` of the 404 for a path no API route matches. Distinct from every
/// object-not-found type, so a client can recognise an endpoint this server
/// version does not have.
pub const ROUTE_NOT_FOUND_PROBLEM_TYPE: &str = "https://temps.sh/probs/route-not-found";

/// `type` of the 405 for a route that exists but not for this method.
pub const METHOD_NOT_ALLOWED_PROBLEM_TYPE: &str = "https://temps.sh/probs/method-not-allowed";

/// `type` of a 4xx for a request body, query string or path the server
/// could not read (malformed JSON, a wrong type, a missing field).
pub const INVALID_REQUEST_PROBLEM_TYPE: &str = "https://temps.sh/probs/invalid-request";

/// Longest request path echoed back in a detail.
const MAX_ECHOED_PATH_CHARS: usize = 256;

/// Largest rejection body this module reads to rewrite it. The extractors'
/// own messages are a sentence or two; anything larger is not one of them
/// and is passed through untouched.
const MAX_REJECTION_BODY_BYTES: u64 = 16 * 1024;

/// Opening words of the messages axum's extractors answer with. Only a
/// `text/plain` 4xx that starts with one of these is rewritten; a handler's
/// own plain-text response is left alone.
const EXTRACTOR_REJECTION_PREFIXES: &[&str] = &[
    "Failed to deserialize",
    "Failed to parse the request body",
    "Failed to buffer the request body",
    "Expected request with `Content-Type",
    "Form requests must have `Content-Type",
    "Invalid URL",
];

/// The 404 for a request no API route matches.
pub fn route_not_found(method: &Method, path: &str) -> Problem {
    route_not_found_with_hint(method, path, None)
}

/// [`route_not_found`] with one more sentence about where the client may
/// have meant to go.
pub fn route_not_found_with_hint(method: &Method, path: &str, hint: Option<&str>) -> Problem {
    let mut detail = format!(
        "No API route matches {method} {}. This Temps server does not provide the endpoint; \
         check the path, or upgrade Temps if the endpoint is newer than this server",
        echoed_path(path)
    );
    if let Some(hint) = hint {
        detail.push_str(". ");
        detail.push_str(hint);
    }
    problemdetails::new(StatusCode::NOT_FOUND)
        .with_type(ROUTE_NOT_FOUND_PROBLEM_TYPE)
        .with_title("Route Not Found")
        .with_detail(detail)
}

/// Fallback handler answering every unmatched request with
/// [`route_not_found`].
pub async fn api_route_not_found(request: Request) -> Response {
    let mut response = route_not_found(request.method(), request.uri().path()).into_response();
    no_store(&mut response);
    response
}

/// Middleware rewriting responses no handler produced into Problem Details:
/// axum's empty 405 for a known path with an unsupported method, and its
/// `text/plain` rejections of an unreadable body, query or path.
///
/// Runs on the console API only (control plane), never on the proxy's
/// request path. It inspects the status and headers of every response and
/// reads a body only for a small `text/plain` 4xx.
pub async fn problem_details_for_unhandled(request: Request, next: Next) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_string();
    let response = next.run(request).await;
    rewrite_unhandled_response(response, &method, &path).await
}

async fn rewrite_unhandled_response(response: Response, method: &Method, path: &str) -> Response {
    let status = response.status();
    if status == StatusCode::METHOD_NOT_ALLOWED && response.body().size_hint().exact() == Some(0) {
        return method_not_allowed(response, method, path);
    }
    if !matches!(
        status,
        StatusCode::BAD_REQUEST
            | StatusCode::PAYLOAD_TOO_LARGE
            | StatusCode::UNSUPPORTED_MEDIA_TYPE
            | StatusCode::UNPROCESSABLE_ENTITY
    ) || !is_plain_text(&response)
    {
        return response;
    }
    let small = response
        .body()
        .size_hint()
        .upper()
        .is_some_and(|upper| upper <= MAX_REJECTION_BODY_BYTES);
    if !small {
        return response;
    }

    let (parts, body) = response.into_parts();
    let bytes = match axum::body::to_bytes(body, MAX_REJECTION_BODY_BYTES as usize).await {
        Ok(bytes) => bytes,
        // The size hint said it fits; a body that lied cannot be replayed,
        // so answer with what the status alone says.
        Err(_) => {
            return invalid_request(status, "The request could not be read".to_string(), None)
                .into_response()
        }
    };
    let text = String::from_utf8_lossy(&bytes);
    if !EXTRACTOR_REJECTION_PREFIXES
        .iter()
        .any(|prefix| text.starts_with(prefix))
    {
        return Response::from_parts(parts, Body::from(bytes));
    }
    let field = rejected_field(&text);
    invalid_request(status, text.into_owned(), field).into_response()
}

fn invalid_request(status: StatusCode, detail: String, field: Option<String>) -> Problem {
    let title = match status {
        StatusCode::UNSUPPORTED_MEDIA_TYPE => "Unsupported Media Type",
        StatusCode::PAYLOAD_TOO_LARGE => "Request Body Too Large",
        _ => "Invalid Request",
    };
    let problem = problemdetails::new(status)
        .with_type(INVALID_REQUEST_PROBLEM_TYPE)
        .with_title(title)
        .with_detail(detail);
    match field {
        Some(field) => problem.with_value("field", field),
        None => problem,
    }
}

/// The router adds the `Allow` header listing the accepted methods after
/// this middleware runs, so the detail points at it rather than repeating it.
fn method_not_allowed(response: Response, method: &Method, path: &str) -> Response {
    let allow = response.headers().get(header::ALLOW).cloned();
    let mut rewritten = problemdetails::new(StatusCode::METHOD_NOT_ALLOWED)
        .with_type(METHOD_NOT_ALLOWED_PROBLEM_TYPE)
        .with_title("Method Not Allowed")
        .with_detail(format!(
            "{} does not accept {method}; the Allow header lists the methods it accepts",
            echoed_path(path)
        ))
        .into_response();
    if let Some(allow) = allow {
        rewritten.headers_mut().insert(header::ALLOW, allow);
    }
    rewritten
}

/// The request field a JSON rejection names, when it names one: the path
/// axum reports before the message (`monitors[0].monitor_id: invalid
/// type ...`), else the field of a `missing field` / `unknown field` message.
fn rejected_field(text: &str) -> Option<String> {
    let message = text
        .strip_prefix("Failed to deserialize the JSON body into the target type: ")
        .or_else(|| text.strip_prefix("Failed to deserialize query string: "))?;
    if let Some((path, _)) = message.split_once(": ") {
        if !path.is_empty() && path != "." && !path.contains(char::is_whitespace) {
            return Some(path.to_string());
        }
    }
    ["missing field `", "unknown field `"]
        .iter()
        .find_map(|marker| {
            let (_, rest) = message.split_once(marker)?;
            let (field, _) = rest.split_once('`')?;
            (!field.is_empty()).then(|| field.to_string())
        })
}

fn is_plain_text(response: &Response) -> bool {
    response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("text/plain"))
}

fn echoed_path(path: &str) -> String {
    if path.chars().count() <= MAX_ECHOED_PATH_CHARS {
        path.to_string()
    } else {
        let mut truncated: String = path.chars().take(MAX_ECHOED_PATH_CHARS).collect();
        truncated.push('…');
        truncated
    }
}

fn no_store(response: &mut Response) {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        routing::{get, post},
        Json, Router,
    };
    use tower::ServiceExt;

    #[derive(serde::Deserialize)]
    #[allow(dead_code, reason = "fields exist only to be deserialized")]
    struct Payload {
        name: String,
        count: i32,
    }

    fn app() -> Router {
        Router::new()
            .route("/api/items", post(|Json(_): Json<Payload>| async { "ok" }))
            .route(
                "/api/plain",
                get(|| async {
                    (
                        StatusCode::BAD_REQUEST,
                        "Failed because the handler said so",
                    )
                }),
            )
            .fallback(api_route_not_found)
            .layer(axum::middleware::from_fn(problem_details_for_unhandled))
    }

    async fn send(request: Request) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
        let response = app().oneshot(request).await.expect("router answers");
        let status = response.status();
        let headers = response.headers().clone();
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("body")
            .to_vec();
        (status, headers, body)
    }

    fn json_post(body: &str) -> Request {
        Request::builder()
            .method(Method::POST)
            .uri("/api/items")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .expect("request")
    }

    fn problem_of(headers: &axum::http::HeaderMap, body: &[u8]) -> serde_json::Value {
        assert_eq!(
            headers
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("application/problem+json")
        );
        serde_json::from_slice(body).expect("problem body is JSON")
    }

    #[tokio::test]
    async fn unknown_route_is_a_route_not_found_problem() {
        let (status, headers, body) = send(
            Request::builder()
                .method(Method::PUT)
                .uri("/api/status-pages/7/password")
                .body(Body::empty())
                .expect("request"),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let problem = problem_of(&headers, &body);
        assert_eq!(problem["type"], ROUTE_NOT_FOUND_PROBLEM_TYPE);
        let detail = problem["detail"].as_str().unwrap_or_default();
        assert!(
            detail.contains("PUT /api/status-pages/7/password"),
            "{detail}"
        );
    }

    #[tokio::test]
    async fn wrong_method_is_a_method_not_allowed_problem_keeping_allow() {
        let (status, headers, body) = send(
            Request::builder()
                .method(Method::DELETE)
                .uri("/api/items")
                .body(Body::empty())
                .expect("request"),
        )
        .await;
        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
        assert!(headers.get(header::ALLOW).is_some());
        let problem = problem_of(&headers, &body);
        assert_eq!(problem["type"], METHOD_NOT_ALLOWED_PROBLEM_TYPE);
        assert!(
            problem["detail"]
                .as_str()
                .is_some_and(|detail| detail.contains("DELETE")),
            "{problem}"
        );
    }

    #[tokio::test]
    async fn json_data_error_is_a_422_problem_naming_the_field() {
        let (status, headers, body) = send(json_post(r#"{"name":"a","count":"many"}"#)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        let problem = problem_of(&headers, &body);
        assert_eq!(problem["type"], INVALID_REQUEST_PROBLEM_TYPE);
        assert_eq!(problem["field"], "count");
        assert!(problem["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("invalid type")));

        let (status, headers, body) = send(json_post(r#"{"count":1}"#)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(problem_of(&headers, &body)["field"], "name");
    }

    #[tokio::test]
    async fn malformed_json_and_wrong_content_type_are_problems() {
        let (status, headers, body) = send(json_post("{not json")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(
            problem_of(&headers, &body)["type"],
            INVALID_REQUEST_PROBLEM_TYPE
        );

        let (status, headers, body) = send(
            Request::builder()
                .method(Method::POST)
                .uri("/api/items")
                .body(Body::from("name=a"))
                .expect("request"),
        )
        .await;
        assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
        assert_eq!(
            problem_of(&headers, &body)["title"],
            "Unsupported Media Type"
        );
    }

    #[tokio::test]
    async fn a_handlers_own_plain_text_error_is_left_alone() {
        let (status, headers, body) = send(
            Request::builder()
                .uri("/api/plain")
                .body(Body::empty())
                .expect("request"),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|value| value.starts_with("text/plain")));
        assert_eq!(body, b"Failed because the handler said so");
    }

    #[test]
    fn long_paths_are_truncated_in_the_detail() {
        let path = format!("/api/{}", "a".repeat(1000));
        let problem = route_not_found(&Method::GET, &path);
        let detail = problem.body["detail"].as_str().unwrap_or_default();
        assert!(detail.len() < 600, "{}", detail.len());
    }
}
