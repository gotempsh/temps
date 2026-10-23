// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The HTTP trigger: one module run per request, WAGI/CGI style.

use std::convert::Infallible;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full, LengthLimitError, Limited};
use hyper::body::Incoming;
use hyper::header::{HeaderName, HeaderValue, CONTENT_TYPE, RETRY_AFTER};
use hyper::http::request::Parts;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;
use tokio::sync::{watch, Semaphore};

use crate::{Executor, RunResult, WasmInstance};

/// Most request headers passed to the guest as `HTTP_*` variables.
const MAX_CGI_HEADERS: usize = 64;
/// Longest header value passed to the guest.
const MAX_CGI_HEADER_VALUE: usize = 4096;
/// Response headers the guest may not set: the server owns framing.
const RESERVED_RESPONSE_HEADERS: &[&str] = &[
    "connection",
    "content-length",
    "transfer-encoding",
    "keep-alive",
    "upgrade",
];

/// Accept connections until `shutdown` fires. At most
/// `max_connections` are open at once: accepting pauses at the bound
/// (backpressure in the kernel's listen queue), never an unbounded queue.
pub(crate) async fn accept_loop(
    listener: TcpListener,
    instance: Arc<WasmInstance>,
    executor: Arc<Executor>,
    mut shutdown: watch::Receiver<bool>,
) {
    let connections = Arc::new(Semaphore::new(executor.config.max_connections));
    loop {
        let permit = tokio::select! {
            permit = Arc::clone(&connections).acquire_owned() => match permit {
                Ok(permit) => permit,
                Err(_) => return,
            },
            _ = shutdown.changed() => return,
        };
        let stream = tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, _peer)) => stream,
                Err(error) => {
                    tracing::warn!(instance = %instance.handle.name, "accepting a connection failed: {error}");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                }
            },
            _ = shutdown.changed() => return,
        };
        let instance = Arc::clone(&instance);
        let executor = Arc::clone(&executor);
        let mut shutdown = shutdown.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let name = instance.handle.name.clone();
            let service = hyper::service::service_fn(move |request| {
                let instance = Arc::clone(&instance);
                let executor = Arc::clone(&executor);
                async move { Ok::<_, Infallible>(handle(&instance, &executor, request).await) }
            });
            let connection = hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(stream), service);
            tokio::select! {
                result = connection => {
                    if let Err(error) = result {
                        tracing::debug!(instance = %name, "a connection ended with an error: {error}");
                    }
                }
                _ = shutdown.changed() => {}
            }
        });
    }
}

fn plain(status: StatusCode, message: &str) -> Response<Full<Bytes>> {
    let mut response = Response::new(Full::new(Bytes::from(format!("{message}\n"))));
    *response.status_mut() = status;
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    response
}

/// Run the module for one request.
pub(crate) async fn handle(
    instance: &WasmInstance,
    executor: &Executor,
    request: Request<Incoming>,
) -> Response<Full<Bytes>> {
    instance.requests.fetch_add(1, Ordering::Relaxed);
    let Ok(permit) = Arc::clone(&instance.runs).try_acquire_owned() else {
        instance.rejected.fetch_add(1, Ordering::Relaxed);
        let mut response = plain(
            StatusCode::SERVICE_UNAVAILABLE,
            "the function is at its concurrency limit; retry shortly",
        );
        response
            .headers_mut()
            .insert(RETRY_AFTER, HeaderValue::from_static("1"));
        return response;
    };
    let (parts, body) = request.into_parts();
    let body = match Limited::new(body, executor.config.max_request_body_bytes)
        .collect()
        .await
    {
        Ok(collected) => collected.to_bytes(),
        Err(error) if error.downcast_ref::<LengthLimitError>().is_some() => {
            return plain(
                StatusCode::PAYLOAD_TOO_LARGE,
                &format!(
                    "the request body exceeds the function's limit of {} bytes",
                    executor.config.max_request_body_bytes
                ),
            );
        }
        Err(error) => {
            return plain(
                StatusCode::BAD_REQUEST,
                &format!("the request body could not be read: {error}"),
            );
        }
    };
    let env = cgi_env(&parts, body.len());
    let outcome = instance.run(executor, body, env).await;
    drop(permit);
    match outcome.result {
        RunResult::Exited(0) => cgi_response(outcome.stdout),
        other => {
            let message = format!("{} {} {}", parts.method, parts.uri.path(), other.describe());
            instance.log(true, message.clone());
            tracing::warn!(instance = %instance.handle.name, "a function run {}", other.describe());
            let status = match other {
                RunResult::OutOfFuel | RunResult::TimedOut => StatusCode::GATEWAY_TIMEOUT,
                RunResult::Exited(_) | RunResult::Failed(_) => StatusCode::BAD_GATEWAY,
            };
            plain(status, &format!("the function {}", other.describe()))
        }
    }
}

/// The CGI variables of a request (RFC 3875 §4.1), bounded.
pub(crate) fn cgi_env(parts: &Parts, content_length: usize) -> Vec<(String, String)> {
    let mut env = vec![
        ("GATEWAY_INTERFACE".to_string(), "CGI/1.1".to_string()),
        (
            "SERVER_PROTOCOL".to_string(),
            format!("{:?}", parts.version),
        ),
        ("REQUEST_METHOD".to_string(), parts.method.to_string()),
        ("PATH_INFO".to_string(), parts.uri.path().to_string()),
        (
            "QUERY_STRING".to_string(),
            parts.uri.query().unwrap_or("").to_string(),
        ),
        ("REQUEST_URI".to_string(), parts.uri.to_string()),
        ("CONTENT_LENGTH".to_string(), content_length.to_string()),
    ];
    if let Some(content_type) = parts
        .headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
    {
        env.push(("CONTENT_TYPE".to_string(), content_type.to_string()));
    }
    for (name, value) in parts.headers.iter().take(MAX_CGI_HEADERS) {
        let Ok(value) = value.to_str() else { continue };
        if value.len() > MAX_CGI_HEADER_VALUE {
            continue;
        }
        let variable = format!(
            "HTTP_{}",
            name.as_str().to_ascii_uppercase().replace('-', "_")
        );
        env.push((variable, value.to_string()));
    }
    env
}

/// The response a guest wrote: CGI headers, a blank line, then the body.
/// Output without a valid header block is the body of a `200`.
pub(crate) fn cgi_response(stdout: Bytes) -> Response<Full<Bytes>> {
    let split = find_header_end(&stdout);
    let parsed = split.and_then(|(headers_end, body_start)| {
        parse_headers(&stdout[..headers_end]).map(|headers| (headers, body_start))
    });
    let Some((headers, body_start)) = parsed else {
        let mut response = Response::new(Full::new(stdout));
        response.headers_mut().insert(
            CONTENT_TYPE,
            HeaderValue::from_static("text/plain; charset=utf-8"),
        );
        return response;
    };
    let mut response = Response::new(Full::new(stdout.slice(body_start..)));
    for (name, value) in headers {
        if name.as_str() == "status" {
            let code = value
                .to_str()
                .ok()
                .and_then(|status| status.split_whitespace().next())
                .and_then(|code| code.parse::<u16>().ok())
                .and_then(|code| StatusCode::from_u16(code).ok());
            match code {
                Some(code) => *response.status_mut() = code,
                None => {
                    return plain(
                        StatusCode::BAD_GATEWAY,
                        "the function wrote an invalid Status header",
                    )
                }
            }
            continue;
        }
        if RESERVED_RESPONSE_HEADERS.contains(&name.as_str()) {
            continue;
        }
        response.headers_mut().append(name, value);
    }
    response
}

/// Where the header block ends and the body starts.
fn find_header_end(output: &[u8]) -> Option<(usize, usize)> {
    let crlf = output.windows(4).position(|w| w == b"\r\n\r\n");
    let lf = output.windows(2).position(|w| w == b"\n\n");
    match (crlf, lf) {
        (Some(crlf), Some(lf)) if lf < crlf => Some((lf, lf + 2)),
        (Some(crlf), _) => Some((crlf, crlf + 4)),
        (None, Some(lf)) => Some((lf, lf + 2)),
        (None, None) => None,
    }
}

/// Every line must be `Name: value`; anything else means there are no
/// headers.
fn parse_headers(block: &[u8]) -> Option<Vec<(HeaderName, HeaderValue)>> {
    let text = std::str::from_utf8(block).ok()?;
    let mut headers = Vec::new();
    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        let (name, value) = line.split_once(':')?;
        let name = HeaderName::from_bytes(name.trim().as_bytes()).ok()?;
        let value = HeaderValue::from_str(value.trim()).ok()?;
        headers.push((name, value));
    }
    (!headers.is_empty()).then_some(headers)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn body_of(response: Response<Full<Bytes>>) -> Bytes {
        response
            .into_body()
            .collect()
            .await
            .map(|collected| collected.to_bytes())
            .unwrap_or_default()
    }

    #[tokio::test]
    async fn a_header_block_sets_the_status_and_headers() {
        let response = cgi_response(Bytes::from_static(
            b"Status: 404 Not Found\nContent-Type: application/json\nX-Fn: 1\n\n{\"missing\":true}",
        ));
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(response.headers()["content-type"], "application/json");
        assert_eq!(response.headers()["x-fn"], "1");
        assert_eq!(
            body_of(response).await,
            Bytes::from_static(b"{\"missing\":true}")
        );
    }

    #[tokio::test]
    async fn output_without_headers_is_a_plain_body() {
        let response = cgi_response(Bytes::from_static(b"hello world\n\nsecond paragraph"));
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()["content-type"],
            "text/plain; charset=utf-8"
        );
        assert_eq!(
            body_of(response).await,
            Bytes::from_static(b"hello world\n\nsecond paragraph")
        );
    }

    #[tokio::test]
    async fn framing_headers_from_the_guest_are_ignored() {
        let response = cgi_response(Bytes::from_static(
            b"Content-Length: 999\r\nTransfer-Encoding: chunked\r\n\r\nok",
        ));
        assert!(response.headers().get("content-length").is_none());
        assert!(response.headers().get("transfer-encoding").is_none());
        assert_eq!(body_of(response).await, Bytes::from_static(b"ok"));
    }

    #[tokio::test]
    async fn an_invalid_status_is_a_bad_gateway() {
        let response = cgi_response(Bytes::from_static(b"Status: teapot\n\nbody"));
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    }

    #[test]
    fn the_request_becomes_cgi_variables() {
        let request = Request::builder()
            .method("POST")
            .uri("/greet?name=ada")
            .header("content-type", "text/plain")
            .header("x-trace", "abc")
            .body(())
            .unwrap();
        let (parts, ()) = request.into_parts();
        let env = cgi_env(&parts, 5);
        let get = |key: &str| {
            env.iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value.as_str())
        };
        assert_eq!(get("REQUEST_METHOD"), Some("POST"));
        assert_eq!(get("PATH_INFO"), Some("/greet"));
        assert_eq!(get("QUERY_STRING"), Some("name=ada"));
        assert_eq!(get("CONTENT_LENGTH"), Some("5"));
        assert_eq!(get("CONTENT_TYPE"), Some("text/plain"));
        assert_eq!(get("HTTP_X_TRACE"), Some("abc"));
    }
}
