// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Temps preview gateway.
//!
//! A tiny TCP-level reverse proxy that lives **inside** the docker network
//! shared with workspace sandbox containers. It listens on `127.0.0.1` only —
//! the host can only reach it via Docker's published port, and the host-side
//! Pingora terminates TLS + enforces Temps SSO before forwarding here.
//!
//! Routing rule: the `Host` header on the first HTTP request determines the
//! upstream. Hostnames take the form `ws-<session_id>-<port>.<anything>`,
//! which the gateway maps to `temps-sandbox-<session_id>:<port>` and resolves
//! via Docker's embedded DNS server (containers on the same user-defined
//! network can address each other by name).
//!
//! After the first read we splice the connection bidirectionally with
//! `tokio::io::copy_bidirectional`, so the proxy is protocol-agnostic for the
//! rest of the connection: works for plain HTTP/1, WebSocket upgrades,
//! Server-Sent Events, gRPC over h2c, and any other request-stream the dev
//! server may speak.
//!
//! Configuration (env vars):
//! - `LISTEN_ADDR`           default `127.0.0.1:8080`
//! - `SANDBOX_HOST_TEMPLATE` default `temps-sandbox-{sid}` — the `{sid}`
//!   token is replaced with the parsed session id.
//! - `MAX_HEADER_BYTES`      default `16384`
//! - `RUST_LOG`              standard tracing filter
//!
//! Build: `cargo build -p temps-preview-gateway --release`
//! Docker image: see `Dockerfile` in this crate.

use std::net::SocketAddr;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tracing::{debug, error, info, warn};

const DEFAULT_LISTEN_ADDR: &str = "127.0.0.1:8080";
const DEFAULT_SANDBOX_HOST_TEMPLATE: &str = "temps-sandbox-{sid}";
const DEFAULT_MAX_HEADER_BYTES: usize = 16 * 1024;
const UPSTREAM_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
struct Config {
    listen_addr: SocketAddr,
    sandbox_host_template: String,
    max_header_bytes: usize,
    /// Shared secret that the host-side Pingora must present in the
    /// `X-Temps-Preview-Token` header. When `None`, the gateway is open
    /// (for tests / curl-from-loopback). When set, every inbound request
    /// must carry the matching token or it is rejected with 403.
    shared_secret: Option<String>,
}

impl Config {
    fn from_env() -> Result<Self> {
        let listen_addr: SocketAddr = std::env::var("LISTEN_ADDR")
            .unwrap_or_else(|_| DEFAULT_LISTEN_ADDR.to_string())
            .parse()
            .context("LISTEN_ADDR is not a valid socket address")?;

        // Security model: the gateway must only be reachable from the host's
        // loopback interface, where the host-side Pingora has already
        // authenticated the request. This is enforced at the *docker port
        // publish* layer (`-p 127.0.0.1:8090:8080`), not by the in-container
        // bind, because inside the container we have to bind 0.0.0.0 for the
        // published port to forward at all.
        //
        // We still refuse anything other than a loopback bind OR 0.0.0.0:
        // - 127.0.0.1 → fine for bare-metal / host-network runs
        // - 0.0.0.0   → fine, but ONLY safe when paired with `-p 127.0.0.1:…`
        // - any other IP → almost certainly an operator mistake
        let ip = listen_addr.ip();
        if !ip.is_loopback() && !ip.is_unspecified() {
            return Err(anyhow!(
                "Refusing to bind {}: LISTEN_ADDR must be a loopback address \
                 or 0.0.0.0 (inside a container with `-p 127.0.0.1:…` publishing)",
                listen_addr
            ));
        }
        if ip.is_unspecified() {
            warn!(
                "Binding {} — this is only safe when the container is started with \
                 `-p 127.0.0.1:<host_port>:{}` so the host loopback is the only ingress",
                listen_addr,
                listen_addr.port()
            );
        }

        let sandbox_host_template = std::env::var("SANDBOX_HOST_TEMPLATE")
            .unwrap_or_else(|_| DEFAULT_SANDBOX_HOST_TEMPLATE.to_string());
        if !sandbox_host_template.contains("{sid}") {
            return Err(anyhow!(
                "SANDBOX_HOST_TEMPLATE must contain the literal token '{{sid}}'"
            ));
        }

        let max_header_bytes = std::env::var("MAX_HEADER_BYTES")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(DEFAULT_MAX_HEADER_BYTES);

        // Require PREVIEW_GATEWAY_SHARED_SECRET. Without it, any local process
        // or other container on the shared bridge network can reach the gateway
        // and proxy traffic to any sandbox, bypassing the host-side preview
        // password wall. We fail fast so misconfigurations are loud.
        let shared_secret = std::env::var("PREVIEW_GATEWAY_SHARED_SECRET")
            .ok()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                anyhow!(
                    "PREVIEW_GATEWAY_SHARED_SECRET is unset or empty. The gateway refuses \
                     to start without a shared secret; the host-side Temps control plane \
                     auto-generates one under TEMPS_DATA_DIR/preview_gateway.secret and \
                     injects it on container creation."
                )
            })?;
        let shared_secret = Some(shared_secret);

        Ok(Self {
            listen_addr,
            sandbox_host_template,
            max_header_bytes,
            shared_secret,
        })
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let config = Config::from_env()?;
    info!(
        listen = %config.listen_addr,
        template = %config.sandbox_host_template,
        "temps-preview-gateway starting"
    );

    let listener = TcpListener::bind(config.listen_addr)
        .await
        .with_context(|| format!("failed to bind {}", config.listen_addr))?;

    loop {
        let (client, peer) = match listener.accept().await {
            Ok(v) => v,
            Err(e) => {
                error!("accept failed: {}", e);
                continue;
            }
        };
        let cfg = config.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_client(client, peer, cfg).await {
                debug!("connection from {} ended: {}", peer, e);
            }
        });
    }
}

async fn handle_client(mut client: TcpStream, peer: SocketAddr, config: Config) -> Result<()> {
    // Read enough bytes to find the end of the HTTP headers (`\r\n\r\n`).
    // We bound the read with both a byte cap and a timeout so a malicious or
    // hung peer can't tie up a worker.
    let mut buf = Vec::with_capacity(2048);
    let header_end = tokio::time::timeout(
        HEADER_READ_TIMEOUT,
        read_until_double_crlf(&mut client, &mut buf, config.max_header_bytes),
    )
    .await
    .map_err(|_| anyhow!("timed out reading request headers"))??;

    // Shared-secret gate: refuse any request that doesn't carry the token
    // the host-side Pingora is configured to inject. This is belt-and-suspenders
    // on top of the docker port-publish bind to 127.0.0.1.
    if let Some(expected) = config.shared_secret.as_deref() {
        let presented = find_header(&buf[..header_end], "x-temps-preview-token");
        let ok = presented
            .map(|t| constant_time_eq(t.as_bytes(), expected.as_bytes()))
            .unwrap_or(false);
        if !ok {
            warn!(
                client = %peer,
                "request rejected: missing or invalid X-Temps-Preview-Token"
            );
            let _ = write_simple_403(
                &mut client,
                "missing or invalid X-Temps-Preview-Token — only the host-side Temps proxy may reach this gateway",
            )
            .await;
            return Err(anyhow!("missing or invalid preview token"));
        }
    }

    let host = match parse_host_header(&buf[..header_end]) {
        Some(h) => h,
        None => {
            warn!(client = %peer, "request rejected: missing or malformed Host header");
            let _ = write_simple_400(
                &mut client,
                "missing Host header — the preview gateway routes on Host header only",
            )
            .await;
            return Err(anyhow!("missing or malformed Host header"));
        }
    };
    info!(client = %peer, host = %host, "inbound preview request");
    let route = match parse_preview_host(host) {
        Some(r) => r,
        None => {
            warn!(
                client = %peer,
                host = %host,
                "request rejected: host does not match ws-<sid>-<port>.<...> scheme"
            );
            let _ = write_simple_400(
                &mut client,
                &format!(
                    "host '{}' does not match required scheme 'ws-<session_id>-<port>.<domain>' \
                     (example: ws-14-3000.localho.st)",
                    host
                ),
            )
            .await;
            return Err(anyhow!(
                "host '{}' does not match ws-<sid>-<port>.<...>",
                host
            ));
        }
    };

    let upstream_host = config
        .sandbox_host_template
        .replace("{sid}", &route.session_id);
    let upstream_addr = format!("{}:{}", upstream_host, route.port);

    info!(
        client = %peer,
        host = %host,
        session_id = %route.session_id,
        port = route.port,
        upstream = %upstream_addr,
        "routing preview connection"
    );

    let mut upstream =
        match tokio::time::timeout(UPSTREAM_CONNECT_TIMEOUT, TcpStream::connect(&upstream_addr))
            .await
        {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => {
                warn!(upstream = %upstream_addr, "upstream connect failed: {}", e);
                let _ = write_simple_502(&mut client, &upstream_addr, &e.to_string()).await;
                return Err(anyhow!("upstream connect failed: {}", e));
            }
            Err(_) => {
                warn!(upstream = %upstream_addr, "upstream connect timed out");
                let _ = write_simple_502(
                    &mut client,
                    &upstream_addr,
                    "timed out connecting to dev server",
                )
                .await;
                return Err(anyhow!("upstream connect timed out"));
            }
        };

    // Replay everything we've already buffered (the entire request head plus
    // anything that may have followed in the same TCP read) before we splice.
    upstream
        .write_all(&buf)
        .await
        .context("failed to forward buffered request to upstream")?;

    // Now bi-directionally splice. Protocol-agnostic from this point on.
    match tokio::io::copy_bidirectional(&mut client, &mut upstream).await {
        Ok((from_client, from_upstream)) => {
            debug!(
                client = %peer,
                from_client_bytes = from_client,
                from_upstream_bytes = from_upstream,
                "connection closed cleanly"
            );
        }
        Err(e) => {
            debug!(client = %peer, "splice ended: {}", e);
        }
    }
    Ok(())
}

/// Read from `stream` into `buf` until the buffer contains `\r\n\r\n` or we
/// hit `max_bytes`. Returns the index immediately after the `\r\n\r\n` sequence
/// (i.e. the start of the request body, if any) on success.
async fn read_until_double_crlf(
    stream: &mut TcpStream,
    buf: &mut Vec<u8>,
    max_bytes: usize,
) -> Result<usize> {
    let mut chunk = [0u8; 1024];
    loop {
        if let Some(idx) = find_double_crlf(buf) {
            return Ok(idx + 4);
        }
        if buf.len() >= max_bytes {
            return Err(anyhow!(
                "request headers exceeded max size of {} bytes",
                max_bytes
            ));
        }
        let n = stream
            .read(&mut chunk)
            .await
            .context("read from client failed")?;
        if n == 0 {
            return Err(anyhow!("client closed connection before sending headers"));
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

fn find_double_crlf(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

/// Extract the value of the first `Host:` header from a buffered HTTP request
/// head. Case-insensitive on the field name.
fn parse_host_header(head: &[u8]) -> Option<&str> {
    let text = std::str::from_utf8(head).ok()?;
    // Skip request line
    let mut lines = text.split("\r\n");
    let _request_line = lines.next()?;
    for line in lines {
        if line.is_empty() {
            break;
        }
        let (name, value) = line.split_once(':')?;
        if name.eq_ignore_ascii_case("host") {
            return Some(value.trim());
        }
    }
    None
}

#[derive(Debug, PartialEq, Eq)]
struct PreviewRoute {
    /// Opaque session identifier extracted from the hostname label.
    ///
    /// Historically this was an `i64` workspace session id. It's now a
    /// string because standalone sandboxes embed the 16-hex suffix of a
    /// `sbx_<hex>` public_id (`ws-<hex>-<port>.domain`) to avoid leaking
    /// enumeration order across tenants.
    ///
    /// The validation rule is: every character must be in
    /// `[A-Za-z0-9]` — DNS labels forbid most other characters anyway,
    /// and constraining the charset protects the upstream host template
    /// against any accidental template injection via the host header.
    session_id: String,
    port: u16,
}

/// Parse a hostname of the form `ws-<session_id>-<port>.<rest>` (or with no
/// `.<rest>` at all). Returns None for anything else, including any host that
/// doesn't start with the literal `ws-` prefix.
///
/// `session_id` may be either:
/// - a positive integer (workspace session id)
/// - a 16-character hex string (standalone sandbox public_id suffix)
///
/// Any other shape is rejected.
fn parse_preview_host(host: &str) -> Option<PreviewRoute> {
    // Strip an optional port suffix on the host header (`example.com:8080`).
    let host_only = host.split(':').next()?;
    let label = host_only.split('.').next()?;
    let rest = label.strip_prefix("ws-")?;
    // rest is `<sid>-<port>`. Split on the LAST '-' so a future label scheme
    // like `ws-<sid>-<label>-<port>` would be a non-breaking change here.
    let (sid_str, port_str) = rest.rsplit_once('-')?;
    let port: u16 = port_str.parse().ok()?;
    if port == 0 {
        return None;
    }
    if sid_str.is_empty() {
        return None;
    }
    if !sid_str.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    // Reject `0` as an integer session id — keeps the old invariant that
    // `session_id > 0` for numeric ids while still accepting hex strings
    // that happen to start with '0'.
    if sid_str.chars().all(|c| c.is_ascii_digit()) {
        let parsed: i64 = sid_str.parse().ok()?;
        if parsed <= 0 {
            return None;
        }
    }
    Some(PreviewRoute {
        session_id: sid_str.to_string(),
        port,
    })
}

/// Case-insensitive lookup of a single header value from a buffered HTTP head.
fn find_header<'a>(head: &'a [u8], name: &str) -> Option<&'a str> {
    let text = std::str::from_utf8(head).ok()?;
    let mut lines = text.split("\r\n");
    let _request_line = lines.next()?;
    for line in lines {
        if line.is_empty() {
            break;
        }
        let (n, v) = line.split_once(':')?;
        if n.eq_ignore_ascii_case(name) {
            return Some(v.trim());
        }
    }
    None
}

/// Constant-time byte comparison to avoid timing leaks on the shared secret.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

async fn write_simple_403(client: &mut TcpStream, detail: &str) -> Result<()> {
    let body = format!("Temps preview gateway: forbidden.\n\n{}\n", detail);
    let response = format!(
        "HTTP/1.1 403 Forbidden\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {}",
        body.len(),
        body
    );
    client.write_all(response.as_bytes()).await?;
    Ok(())
}

async fn write_simple_400(client: &mut TcpStream, detail: &str) -> Result<()> {
    let body = format!("Temps preview gateway: bad request.\n\n{}\n", detail);
    let response = format!(
        "HTTP/1.1 400 Bad Request\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {}",
        body.len(),
        body
    );
    client.write_all(response.as_bytes()).await?;
    Ok(())
}

async fn write_simple_502(client: &mut TcpStream, upstream: &str, detail: &str) -> Result<()> {
    let body = format!(
        "Temps preview gateway: upstream {} unavailable.\n\n{}\n",
        upstream, detail
    );
    let response = format!(
        "HTTP/1.1 502 Bad Gateway\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {}",
        body.len(),
        body
    );
    client.write_all(response.as_bytes()).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_basic_preview_host() {
        let r = parse_preview_host("ws-11-3000.localho.st").unwrap();
        assert_eq!(
            r,
            PreviewRoute {
                session_id: "11".to_string(),
                port: 3000
            }
        );
    }

    #[test]
    fn parses_preview_host_with_explicit_port_suffix() {
        let r = parse_preview_host("ws-11-3000.localho.st:8080").unwrap();
        assert_eq!(
            r,
            PreviewRoute {
                session_id: "11".to_string(),
                port: 3000
            }
        );
    }

    #[test]
    fn parses_preview_host_with_no_dot() {
        let r = parse_preview_host("ws-7-5173").unwrap();
        assert_eq!(
            r,
            PreviewRoute {
                session_id: "7".to_string(),
                port: 5173
            }
        );
    }

    #[test]
    fn parses_hex_public_id_suffix() {
        // Standalone sandboxes embed the 16-char hex suffix of their
        // `sbx_<hex>` public_id — the gateway must route these too.
        let r = parse_preview_host("ws-abcd1234ef567890-3000.localho.st").unwrap();
        assert_eq!(
            r,
            PreviewRoute {
                session_id: "abcd1234ef567890".to_string(),
                port: 3000
            }
        );
    }

    #[test]
    fn rejects_non_ws_prefix() {
        assert!(parse_preview_host("api.example.com").is_none());
        assert!(parse_preview_host("preview-11-3000.localho.st").is_none());
    }

    #[test]
    fn rejects_zero_port_or_session() {
        assert!(parse_preview_host("ws-0-3000.localho.st").is_none());
        assert!(parse_preview_host("ws-11-0.localho.st").is_none());
    }

    #[test]
    fn rejects_invalid_chars_in_sid() {
        // Underscores and other special chars are forbidden — only
        // `[A-Za-z0-9]` allowed.
        assert!(parse_preview_host("ws-sbx_abcdef-3000.localho.st").is_none());
        assert!(parse_preview_host("ws-11-xxx.localho.st").is_none());
    }

    #[test]
    fn future_label_scheme_currently_rejected() {
        // ws-<sid>-<label>-<port> is not yet supported — rsplit_once('-')
        // would give sid="11-web" which contains a '-' (not alphanumeric).
        // When we add label support, update parse_preview_host and this
        // test together.
        assert!(parse_preview_host("ws-11-web-3000.localho.st").is_none());
    }

    #[test]
    fn parses_host_header_case_insensitive() {
        let head = b"GET / HTTP/1.1\r\nhOsT: ws-11-3000.localho.st\r\n\r\n";
        assert_eq!(parse_host_header(head), Some("ws-11-3000.localho.st"));
    }

    #[test]
    fn parses_host_header_with_other_headers_first() {
        let head = b"GET / HTTP/1.1\r\nUser-Agent: x\r\nHost: ws-1-2.test\r\n\r\n";
        assert_eq!(parse_host_header(head), Some("ws-1-2.test"));
    }

    #[test]
    fn missing_host_header_returns_none() {
        let head = b"GET / HTTP/1.1\r\nUser-Agent: x\r\n\r\n";
        assert_eq!(parse_host_header(head), None);
    }

    #[test]
    fn finds_double_crlf() {
        let buf = b"GET / HTTP/1.1\r\nHost: x\r\n\r\nbody";
        assert_eq!(find_double_crlf(buf), Some(23));
    }
}
