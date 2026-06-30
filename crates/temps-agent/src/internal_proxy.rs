//! Internal edge proxy bound to the worker's overlay bridge gateway.
//!
//! Containers on this worker resolve `<env>.<project>.temps.local` to
//! the bridge gateway IP via the per-node Hickory resolver. They open a
//! plain HTTP connection on `:80`. This module is what answers.
//!
//! ## Routing decision
//!
//! Every decision is read from [`crate::route_store::RouteStore`],
//! which the [`crate::route_sync_client::RouteSyncClient`] mirrors
//! from the CP. There is no DB call on the request path; lookup is
//! `HashMap::get` under a brief read lock.
//!
//! ## Backend selection
//!
//! Random pick from the host's healthy backend list. No connection
//! affinity in v1 — most internal traffic is short-lived HTTP and
//! distributing each request independently keeps the math simple. If
//! one worker is down, only ~1/N of requests fail until the next
//! sync round drops it from the list; the client retries and lands on
//! a different backend on the next try.
//!
//! ## Hop-by-hop headers
//!
//! Stripped per RFC 7230 §6.1: `connection`, `keep-alive`,
//! `proxy-authenticate`, `proxy-authorization`, `te`, `trailers`,
//! `transfer-encoding`, `upgrade`. The proxy adds standard
//! `x-forwarded-*` headers and a `x-temps-deployment-id` for log
//! correlation.
//!
//! ## Errors mapped to HTTP
//!
//! - No matching host → `404` with explanatory body.
//! - Host has no healthy backends → `503`.
//! - Upstream connection failure → `502`, retried up to 3 times against
//!   different backends before giving up.
//! - Upstream timeout → `504`.
//!
//! ## Listen scope
//!
//! Always binds to `<bridge_ip>:80`. Never `0.0.0.0`. Never published
//! via Docker. The only callers are processes inside the overlay
//! network, which is precisely the trust boundary we want.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use rand::seq::SliceRandom;
use tokio::net::TcpListener;
use tokio::sync::Notify;
use tracing::{debug, error, info, warn};

use crate::route_store::{RouteEntry, SharedRouteStore};

/// Hop-by-hop headers per RFC 7230 §6.1. Lowercased so comparison is
/// trivial (axum normalises, but be explicit).
const HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailers",
    "transfer-encoding",
    "upgrade",
    "host",
];

/// Per-attempt upstream timeout. Internal-zone calls are between
/// containers on the same overlay; if 30s isn't enough something is
/// already wrong elsewhere.
const UPSTREAM_TIMEOUT: Duration = Duration::from_secs(30);

/// Maximum upstream retry attempts (across distinct backends) on
/// connect failure before returning 502.
const MAX_RETRIES: usize = 3;

/// Shared HTTP client + route store for the proxy's request handler.
struct ProxyState {
    client: reqwest::Client,
    store: SharedRouteStore,
}

/// Spawn the proxy on `<bridge_ip>:80`. Returns once the listener is
/// bound; the request loop runs in a background task and exits when
/// `shutdown` is notified.
///
/// Idempotent at the call site: callers should only invoke this once
/// per agent process.
pub async fn spawn(
    bridge_ip: IpAddr,
    port: u16,
    store: SharedRouteStore,
    shutdown: Arc<Notify>,
) -> std::io::Result<()> {
    let addr = SocketAddr::new(bridge_ip, port);
    let listener = TcpListener::bind(addr).await?;
    info!(%addr, "internal edge proxy listening");

    let client = reqwest::Client::builder()
        .timeout(UPSTREAM_TIMEOUT)
        // No redirect-following: internal traffic should be
        // self-contained, and following a redirect to a public URL
        // from inside the overlay is a foot-gun.
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| std::io::Error::other(e.to_string()))?;

    let state = Arc::new(ProxyState { client, store });
    let app = axum::Router::new().fallback(handle).with_state(state);

    tokio::spawn(async move {
        let server = axum::serve(listener, app).with_graceful_shutdown(async move {
            shutdown.notified().await;
            info!("internal edge proxy shutting down");
        });
        if let Err(e) = server.await {
            error!(error = %e, "internal edge proxy exited with error");
        }
    });
    Ok(())
}

async fn handle(State(state): State<Arc<ProxyState>>, req: Request) -> Response {
    let host = match extract_host(req.headers()) {
        Some(h) => h,
        None => {
            return error_body(StatusCode::BAD_REQUEST, "missing or malformed Host header");
        }
    };

    // Reject non-temps.local up front. The proxy is not a generic
    // forwarder; matching only our internal zone makes accidental
    // misconfiguration (e.g. someone CNAME'd a public name to our
    // bridge) fail loudly instead of forwarding traffic somewhere it
    // shouldn't go.
    if !host.ends_with(".temps.local") {
        return error_body(
            StatusCode::NOT_FOUND,
            "internal proxy serves *.temps.local only",
        );
    }

    let entry = match state.store.lookup(&host) {
        Some(e) => e,
        None => {
            warn!(%host, "no route in local store");
            return error_body(StatusCode::NOT_FOUND, "no internal route for this host");
        }
    };

    if entry.backends.is_empty() {
        warn!(%host, "host has no backends");
        return error_body(StatusCode::SERVICE_UNAVAILABLE, "no live backends");
    }

    // WebSocket / generic protocol upgrade. Reqwest's HTTP client
    // doesn't tunnel after a 101 response — it returns the response
    // headers and considers the request done — so we have to take the
    // upgrade off the wire ourselves: open a raw TCP connection to a
    // backend, replay the request line + headers exactly, then bridge
    // bytes between the upgraded client socket and that backend
    // socket. Works for `Upgrade: websocket`, but also for any other
    // RFC 7230 Upgrade target (HTTP/2 prior knowledge isn't supported
    // here — internal traffic is plain HTTP/1.1).
    if is_upgrade_request(req.headers()) {
        return proxy_upgrade(&entry, req).await;
    }

    proxy_with_retries(&state, &entry, req).await
}

/// True if the client requested a protocol upgrade. We check both the
/// `connection: upgrade` token (per RFC 7230 §6.7) and the presence of
/// `upgrade:` — the websocket case must satisfy both.
fn is_upgrade_request(headers: &HeaderMap) -> bool {
    let conn = headers
        .get("connection")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    let has_upgrade_token = conn.split(',').any(|t| t.trim() == "upgrade");
    has_upgrade_token && headers.contains_key("upgrade")
}

/// Tunnel an HTTP/1.1 Upgrade through to a backend. Picks one backend
/// at random (no retries — upgraded connections are unique to a
/// specific backend, retrying after a partial handshake would mean
/// duplicating client bytes). Forwards the original request line and
/// headers (minus `Host`, which we override so the backend sees the
/// real downstream host), reads the backend's status line + headers
/// up to the empty line, and pipes bytes bidirectionally until either
/// side closes.
async fn proxy_upgrade(entry: &RouteEntry, req: Request) -> Response {
    use rand::seq::SliceRandom;
    let backend = {
        let mut rng = rand::thread_rng();
        entry.backends.choose(&mut rng).cloned()
    };
    let Some(backend) = backend else {
        return error_body(StatusCode::SERVICE_UNAVAILABLE, "no live backends");
    };

    let method = req.method().clone();
    let uri = req.uri().clone();
    let path_and_query = uri
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or("/")
        .to_string();
    let headers = req.headers().clone();
    let original_host = headers
        .get("host")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    // Dial backend up front. If this fails the client sees a 502
    // before its own upgrade attempt is committed, which is the
    // friendly outcome.
    let mut backend_sock = match tokio::net::TcpStream::connect(&backend.address).await {
        Ok(s) => s,
        Err(e) => {
            warn!(backend = %backend.address, error = %e, "ws upgrade backend connect failed");
            return error_body(
                StatusCode::BAD_GATEWAY,
                &format!("upstream {}: {}", backend.address, e),
            );
        }
    };

    // Build the request as raw bytes. Manually so we don't have to
    // wrestle with hyper's typed encoder for this narrow path.
    let mut wire = format!("{} {} HTTP/1.1\r\n", method.as_str(), path_and_query).into_bytes();
    let mut sent_host = false;
    for (name, value) in headers.iter() {
        let name_str = name.as_str();
        // Drop hop-by-hop except `connection` and `upgrade`, which the
        // upgrade flow needs preserved verbatim per RFC 6455 §4.
        if matches!(
            name_str,
            "proxy-authenticate" | "proxy-authorization" | "te" | "trailers" | "transfer-encoding"
        ) {
            continue;
        }
        // Strip client-supplied forwarding/identity headers (ADR-020 WS-3 /
        // netiso-5). The proxy sets `x-forwarded-*` and `x-temps-deployment-id`
        // authoritatively below; passing inbound copies through would let any
        // overlay client spoof its source host/proto or impersonate another
        // deployment to the backend.
        if name_str == "forwarded"
            || name_str.starts_with("x-forwarded-")
            || name_str.starts_with("x-temps-")
        {
            continue;
        }
        if name_str == "host" {
            sent_host = true;
        }
        wire.extend_from_slice(name_str.as_bytes());
        wire.extend_from_slice(b": ");
        wire.extend_from_slice(value.as_bytes());
        wire.extend_from_slice(b"\r\n");
    }
    if !sent_host {
        if let Some(h) = &original_host {
            wire.extend_from_slice(format!("host: {}\r\n", h).as_bytes());
        }
    }
    if let Some(deployment_id) = entry.deployment_id {
        wire.extend_from_slice(format!("x-temps-deployment-id: {}\r\n", deployment_id).as_bytes());
    }
    if let Some(h) = &original_host {
        wire.extend_from_slice(format!("x-forwarded-host: {}\r\n", h).as_bytes());
    }
    wire.extend_from_slice(b"x-forwarded-proto: http\r\n\r\n");

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    if let Err(e) = backend_sock.write_all(&wire).await {
        warn!(backend = %backend.address, error = %e, "ws backend write request failed");
        return error_body(StatusCode::BAD_GATEWAY, "upstream write failed");
    }

    // Read the backend's response headers. We only need to peek at
    // the status line — anything in the 100s/200s with the upgrade
    // sequence completes the tunnel; anything else we relay verbatim
    // and stop. We hand-parse to avoid pulling in another HTTP
    // parser; the response shape is "STATUS\r\nHEADER\r\n...\r\n\r\n".
    let mut buf = Vec::with_capacity(4096);
    let mut tmp = [0u8; 1024];
    let header_end = loop {
        match backend_sock.read(&mut tmp).await {
            Ok(0) => {
                warn!(backend = %backend.address, "backend closed before headers");
                return error_body(StatusCode::BAD_GATEWAY, "upstream closed early");
            }
            Ok(n) => {
                buf.extend_from_slice(&tmp[..n]);
                if let Some(idx) = find_header_end(&buf) {
                    break idx;
                }
                if buf.len() > 64 * 1024 {
                    warn!("ws backend headers exceed 64KiB; aborting");
                    return error_body(StatusCode::BAD_GATEWAY, "upstream header too large");
                }
            }
            Err(e) => {
                warn!(backend = %backend.address, error = %e, "ws backend read failed");
                return error_body(StatusCode::BAD_GATEWAY, "upstream read failed");
            }
        }
    };
    let header_bytes = buf[..header_end].to_vec();
    let leftover = buf[header_end..].to_vec();

    // Parse status code + parse the header block back into typed
    // headers we can hand to axum.
    let (status, headers_out) = match parse_response_head(&header_bytes) {
        Ok(v) => v,
        Err(e) => {
            warn!(error = %e, "ws backend response parse failed");
            return error_body(StatusCode::BAD_GATEWAY, "upstream malformed");
        }
    };

    // Non-101 ⇒ backend declined the upgrade; relay status+headers
    // and any leftover body, then close. No bidirectional tunnel.
    if status != StatusCode::SWITCHING_PROTOCOLS {
        let mut resp = Response::new(Body::from(leftover));
        *resp.status_mut() = status;
        for (name, value) in headers_out.iter() {
            if HOP_BY_HOP.contains(&name.as_str()) {
                continue;
            }
            resp.headers_mut().insert(name.clone(), value.clone());
        }
        return resp;
    }

    // 101: upgrade the *client* connection so axum hands us the raw
    // socket, then bridge bytes both ways. The `on_upgrade` future
    // resolves once axum's response (the 101 we're about to build)
    // has been written.
    let on_upgrade = hyper::upgrade::on(req);
    let response_bytes = headers_out;
    // Build the 101 response we send to the client. Mirror the
    // backend's headers verbatim — Sec-WebSocket-Accept comes from
    // there and must round-trip unchanged.
    let mut client_resp = Response::new(Body::empty());
    *client_resp.status_mut() = StatusCode::SWITCHING_PROTOCOLS;
    for (name, value) in response_bytes.iter() {
        client_resp
            .headers_mut()
            .insert(name.clone(), value.clone());
    }

    // Spawn the bidirectional copy after the response is sent. We
    // need to detach the futures since axum returns the response
    // before the tunnel starts.
    tokio::spawn(async move {
        let upgraded = match on_upgrade.await {
            Ok(u) => u,
            Err(e) => {
                warn!(error = %e, "ws client upgrade failed");
                return;
            }
        };
        // hyper::upgrade::Upgraded uses hyper's I/O traits, not
        // tokio's. Wrap with TokioIo so `copy_bidirectional` and
        // `write_all` work.
        let mut client_sock = hyper_util::rt::TokioIo::new(upgraded);
        // Replay any leftover bytes the backend already sent past the
        // header terminator (ws frames often arrive in the same
        // packet as the 101 from the upstream).
        if !leftover.is_empty() {
            if let Err(e) = client_sock.write_all(&leftover).await {
                warn!(error = %e, "ws leftover write to client failed");
                return;
            }
        }
        match tokio::io::copy_bidirectional(&mut client_sock, &mut backend_sock).await {
            Ok((up, down)) => {
                debug!(up_bytes = up, down_bytes = down, "ws tunnel closed");
            }
            Err(e) => {
                debug!(error = %e, "ws tunnel ended with error");
            }
        }
    });

    client_resp
}

/// Find the position immediately after the first `\r\n\r\n` in `buf`.
/// Returns `None` if the terminator isn't yet present.
fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4)
}

/// Parse a raw HTTP response head (status line + headers) into a
/// typed `(StatusCode, HeaderMap)`. Strict about CRLF — internal
/// servers all conform.
fn parse_response_head(buf: &[u8]) -> Result<(StatusCode, HeaderMap), String> {
    let text = std::str::from_utf8(buf).map_err(|e| format!("non-utf8: {e}"))?;
    let mut lines = text.split("\r\n");
    let status_line = lines.next().ok_or("empty response")?;
    // "HTTP/1.1 101 Switching Protocols"
    let mut parts = status_line.splitn(3, ' ');
    let _version = parts.next().ok_or("missing version")?;
    let code_str = parts.next().ok_or("missing code")?;
    let code: u16 = code_str.parse().map_err(|e| format!("bad code: {e}"))?;
    let status = StatusCode::from_u16(code).map_err(|e| format!("bad status: {e}"))?;

    let mut headers = HeaderMap::new();
    for line in lines {
        if line.is_empty() {
            break;
        }
        let (name, value) = line.split_once(':').ok_or("malformed header")?;
        let name_trim = name.trim();
        let value_trim = value.trim();
        if name_trim.is_empty() {
            continue;
        }
        let name = HeaderName::from_bytes(name_trim.as_bytes())
            .map_err(|e| format!("bad header name {name_trim:?}: {e}"))?;
        let value =
            HeaderValue::from_str(value_trim).map_err(|e| format!("bad header value: {e}"))?;
        headers.append(name, value);
    }
    Ok((status, headers))
}

async fn proxy_with_retries(state: &ProxyState, entry: &RouteEntry, req: Request) -> Response {
    // Snapshot method/uri/headers up front; we may need to clone
    // body bytes if we want true retries on body-bearing requests.
    // For v1 we only retry idempotent methods (GET/HEAD/OPTIONS) so
    // the body issue is moot. Methods with bodies get one shot.
    let method = req.method().clone();
    let uri = req.uri().clone();
    let headers = req.headers().clone();
    let retryable = matches!(
        method,
        axum::http::Method::GET | axum::http::Method::HEAD | axum::http::Method::OPTIONS
    );

    // Pre-shuffle the backend order so attempts hit distinct
    // backends. We don't want an unhealthy first-in-list backend to
    // hot-spot every request.
    let mut order: Vec<usize> = (0..entry.backends.len()).collect();
    {
        let mut rng = rand::thread_rng();
        order.shuffle(&mut rng);
    }
    let attempts = if retryable {
        MAX_RETRIES.min(order.len())
    } else {
        1
    };

    // Body has to be consumed once. If we want to retry across
    // backends we'd have to buffer it; for v1 we accept "method with
    // body = single attempt" and let clients retry at their layer.
    let body = req.into_body();

    if attempts <= 1 || !retryable {
        let backend = &entry.backends[order[0]];
        return forward_once(state, entry, backend, &method, &uri, &headers, body).await;
    }

    // Buffer the body so we can replay across attempts. For
    // GET/HEAD/OPTIONS the body is normally empty; this is a no-op.
    let bytes = match axum::body::to_bytes(body, usize::MAX).await {
        Ok(b) => b,
        Err(e) => {
            error!(error = %e, "failed to buffer request body");
            return error_body(StatusCode::BAD_REQUEST, "request body read failed");
        }
    };

    let mut last_err: Option<String> = None;
    for idx in order.iter().take(attempts) {
        let backend = &entry.backends[*idx];
        let body = Body::from(bytes.clone());
        let resp = forward_once(state, entry, backend, &method, &uri, &headers, body).await;
        // Treat 502/504 as retryable transport errors.
        let status = resp.status();
        if status == StatusCode::BAD_GATEWAY || status == StatusCode::GATEWAY_TIMEOUT {
            last_err = Some(format!(
                "backend {} returned {}",
                backend.address,
                status.as_u16()
            ));
            continue;
        }
        return resp;
    }

    warn!(
        host = %entry.host,
        attempts,
        last_err = ?last_err,
        "all backends failed",
    );
    error_body(
        StatusCode::BAD_GATEWAY,
        &format!(
            "all {} backend(s) for {} failed: {}",
            attempts,
            entry.host,
            last_err.unwrap_or_default()
        ),
    )
}

async fn forward_once(
    state: &ProxyState,
    entry: &RouteEntry,
    backend: &crate::route_store::RouteBackend,
    method: &axum::http::Method,
    uri: &Uri,
    headers: &HeaderMap,
    body: Body,
) -> Response {
    let path_and_query = uri
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or(uri.path());
    let upstream_url = format!("http://{}{}", backend.address, path_and_query);

    debug!(
        host = %entry.host,
        upstream = %upstream_url,
        method = %method,
        "forwarding internal request"
    );

    // Build the outbound request. We translate axum's Method/HeaderMap
    // into reqwest's; reqwest re-uses the http crate types so this is
    // a parse round-trip rather than reflection.
    let mut builder = state.client.request(method.clone(), &upstream_url);

    for (name, value) in headers.iter() {
        if HOP_BY_HOP.contains(&name.as_str()) {
            continue;
        }
        builder = builder.header(name.as_str(), value);
    }

    // X-Forwarded-* — give backends visibility into the original
    // request shape. Internal proxy is plain HTTP only, so proto is
    // always "http".
    builder = builder.header("x-forwarded-proto", "http");
    if let Some(orig_host) = headers.get("host").and_then(|v| v.to_str().ok()) {
        builder = builder.header("x-forwarded-host", orig_host);
    }
    if let Some(deployment_id) = entry.deployment_id {
        builder = builder.header("x-temps-deployment-id", deployment_id.to_string());
    }

    // Stream the request body. reqwest accepts a `Body` built from a
    // bytes Stream; we adapt axum's Body via http_body_util.
    let stream = body_to_stream(body);
    builder = builder.body(reqwest::Body::wrap_stream(stream));

    let upstream = match builder.send().await {
        Ok(r) => r,
        Err(e) => {
            warn!(
                upstream = %upstream_url,
                error = %e,
                "upstream connect failed"
            );
            // reqwest doesn't expose `is_timeout` reliably across
            // versions; rely on the surfaced display string.
            let timeout =
                e.is_timeout() || format!("{e}").to_ascii_lowercase().contains("timed out");
            let status = if timeout {
                StatusCode::GATEWAY_TIMEOUT
            } else {
                StatusCode::BAD_GATEWAY
            };
            return error_body(status, &format!("upstream {}: {}", backend.address, e));
        }
    };

    // Capture status + headers BEFORE consuming the response body
    // stream — `bytes_stream(self)` takes the response by value.
    let status = upstream.status();
    let upstream_headers_owned = upstream.headers().clone();

    let mut out = Response::new(Body::from_stream(upstream.bytes_stream()));
    *out.status_mut() = status;
    for (name, value) in upstream_headers_owned.iter() {
        if HOP_BY_HOP.contains(&name.as_str()) {
            continue;
        }
        out.headers_mut().insert(name.clone(), value.clone());
    }
    out
}

/// Convert axum's `Body` into a stream of `Result<Bytes, _>` that
/// `reqwest::Body::wrap_stream` accepts. Backpressure preserved.
fn body_to_stream(
    body: Body,
) -> impl futures::Stream<Item = Result<bytes::Bytes, std::io::Error>> + Send + 'static {
    use futures::StreamExt;
    use http_body_util::BodyStream;
    BodyStream::new(body).filter_map(|frame| async move {
        match frame {
            Ok(f) => f.into_data().ok().map(Ok),
            Err(e) => Some(Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                e.to_string(),
            ))),
        }
    })
}

fn extract_host(headers: &HeaderMap) -> Option<String> {
    let h = headers.get("host")?.to_str().ok()?;
    // Strip port suffix if present (`example.com:8080` → `example.com`).
    let host = h.split(':').next().unwrap_or(h);
    Some(host.to_ascii_lowercase())
}

fn error_body(status: StatusCode, message: &str) -> Response {
    let body = format!("{}: {}\n", status.canonical_reason().unwrap_or(""), message);
    let mut resp = (status, body).into_response();
    resp.headers_mut().insert(
        HeaderName::from_static("content-type"),
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    resp
}
