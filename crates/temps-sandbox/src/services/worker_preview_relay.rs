// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Preview URLs of sandboxes on worker nodes.
//!
//! The preview gateway splices `ws-<hex>-<port>.<domain>` connections into
//! sandboxes on this host through Docker DNS. A sandbox on a worker is not on
//! any network this host can dial, so the proxy hands those connections here
//! instead, after the same preview authentication. The relay speaks the
//! gateway's protocol: it reads the request head, checks the proxy's token,
//! takes the sandbox and port from `Host`, strips the token, and splices the
//! connection into a `port` byte channel the worker's agent opens to that port
//! inside the sandbox — over the mTLS connection the terminal already uses.
//!
//! It listens on a loopback port picked at startup and registers its address
//! and token in [`temps_core::sandbox_preview_relay`] for the proxy; there is
//! nothing to configure. Concurrent connections are capped at
//! [`MAX_CONNECTIONS`]; a connection over the cap is answered 503 at once.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter};
use temps_core::sandbox_preview_relay::{
    register_worker_preview_relay, WorkerPreviewRelay, WORKER_PREVIEW_RELAY_TOKEN_HEADER,
};
use temps_entities::sandboxes;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;

use super::registry::StandaloneSandboxRegistry;

/// Largest request head read before routing.
const MAX_HEADER_BYTES: usize = 16 * 1024;
/// How long a client has to send its request head.
const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(10);
/// How long the worker has to open the channel to the sandbox port.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// Concurrent preview connections to worker sandboxes. Each one holds a
/// WebSocket to a worker and a `docker exec` there.
pub const MAX_CONNECTIONS: usize = 256;

/// Why one preview connection was answered with an error instead of spliced.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RelayRefusal {
    #[error(
        "missing or invalid preview token: only the Temps proxy may use the worker preview relay"
    )]
    Unauthorized,
    #[error("malformed preview request: {reason}")]
    BadRequest { reason: String },
    #[error("sandbox sbx_{hex} does not exist or was destroyed")]
    SandboxNotFound { hex: String },
    #[error("sandbox {public_id} runs on the control plane; its previews go through the preview gateway, not the worker relay")]
    NotOnWorker { public_id: String },
    #[error("port {port} of sandbox {public_id} on node {node_id} could not be reached: {reason}")]
    Unreachable {
        public_id: String,
        node_id: i32,
        port: u16,
        reason: String,
    },
    #[error("too many preview connections to worker sandboxes ({limit} open); retry shortly")]
    Saturated { limit: usize },
}

impl RelayRefusal {
    fn status_line(&self) -> &'static str {
        match self {
            Self::Unauthorized => "403 Forbidden",
            Self::BadRequest { .. } => "400 Bad Request",
            Self::SandboxNotFound { .. } => "404 Not Found",
            Self::NotOnWorker { .. } | Self::Unreachable { .. } => "502 Bad Gateway",
            Self::Saturated { .. } => "503 Service Unavailable",
        }
    }
}

/// The relay's shared state; one per process.
pub struct WorkerPreviewRelayServer {
    db: Arc<DatabaseConnection>,
    registry: Arc<StandaloneSandboxRegistry>,
    token: String,
    slots: Arc<Semaphore>,
}

impl WorkerPreviewRelayServer {
    pub fn new(db: Arc<DatabaseConnection>, registry: Arc<StandaloneSandboxRegistry>) -> Self {
        use rand::Rng;
        let mut secret = [0u8; 32];
        rand::rng().fill_bytes(&mut secret);
        Self {
            db,
            registry,
            token: hex::encode(secret),
            slots: Arc::new(Semaphore::new(MAX_CONNECTIONS)),
        }
    }

    /// Bind a loopback port, register it for the proxy and serve until the
    /// process exits.
    pub async fn start(self: Arc<Self>) -> std::io::Result<SocketAddr> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let address = listener.local_addr()?;
        if !register_worker_preview_relay(WorkerPreviewRelay {
            address,
            token: self.token.clone(),
        }) {
            tracing::warn!(
                %address,
                "A worker preview relay was already registered in this process; this one stays unused"
            );
        }
        tracing::info!(%address, "Worker sandbox preview relay listening");
        tokio::spawn(async move {
            loop {
                let (client, peer) = match listener.accept().await {
                    Ok(accepted) => accepted,
                    Err(error) => {
                        tracing::warn!(%error, "Worker preview relay accept failed");
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        continue;
                    }
                };
                let relay = self.clone();
                tokio::spawn(async move { relay.serve(client, peer).await });
            }
        });
        Ok(address)
    }

    async fn serve(&self, mut client: TcpStream, peer: SocketAddr) {
        let Ok(_slot) = self.slots.clone().try_acquire_owned() else {
            let refusal = RelayRefusal::Saturated {
                limit: MAX_CONNECTIONS,
            };
            tracing::warn!(%peer, "{refusal}");
            write_refusal(&mut client, &refusal).await;
            return;
        };
        if let Err(refusal) = self.relay(&mut client).await {
            match &refusal {
                RelayRefusal::Unauthorized | RelayRefusal::BadRequest { .. } => {
                    tracing::warn!(%peer, "Worker preview relay refused a connection: {refusal}")
                }
                _ => tracing::info!(%peer, "Worker preview relay refused a connection: {refusal}"),
            }
            write_refusal(&mut client, &refusal).await;
        }
    }

    /// Route one connection and splice it. An `Err` means nothing reached
    /// the client yet, so the caller can still answer with a status.
    async fn relay(&self, client: &mut TcpStream) -> Result<(), RelayRefusal> {
        let mut buffered = Vec::with_capacity(2048);
        let header_end = tokio::time::timeout(
            HEADER_READ_TIMEOUT,
            read_request_head(client, &mut buffered),
        )
        .await
        .map_err(|_| RelayRefusal::BadRequest {
            reason: "timed out reading the request head".to_string(),
        })??;
        let request = PreviewRequest::parse(&buffered, header_end, &self.token)?;

        let public_id = format!("sbx_{}", request.hex);
        let row = sandboxes::Entity::find()
            .filter(sandboxes::Column::PublicId.eq(public_id.clone()))
            .one(self.db.as_ref())
            .await
            .map_err(|error| RelayRefusal::Unreachable {
                public_id: public_id.clone(),
                node_id: 0,
                port: request.port,
                reason: format!("loading the sandbox failed: {error}"),
            })?
            .filter(|row| row.status != "destroyed")
            .ok_or_else(|| RelayRefusal::SandboxNotFound {
                hex: request.hex.clone(),
            })?;
        let Some(node_id) = row.node_id else {
            return Err(RelayRefusal::NotOnWorker { public_id });
        };
        let unreachable = |reason: String| RelayRefusal::Unreachable {
            public_id: public_id.clone(),
            node_id,
            port: request.port,
            reason,
        };

        let handle = self
            .registry
            .get_or_recover(row.id, &row.public_id)
            .await
            .map_err(|error| unreachable(error.to_string()))?;
        let attachment = tokio::time::timeout(
            CONNECT_TIMEOUT,
            self.registry.provider().connect_port(&handle, request.port),
        )
        .await
        .map_err(|_| {
            unreachable(format!(
                "the worker did not open the port within {}s",
                CONNECT_TIMEOUT.as_secs()
            ))
        })?
        .map_err(|error| unreachable(error.to_string()))?;

        tracing::debug!(
            sandbox_id = %public_id,
            node_id,
            port = request.port,
            "Relaying a preview connection to a worker sandbox"
        );
        splice(client, attachment, request.forwarded)
            .await
            .map_err(|reason| unreachable(explain_channel_failure(request.port, reason)))
    }
}

/// A request head the relay accepted, with the token stripped.
#[derive(Debug, PartialEq, Eq)]
struct PreviewRequest {
    hex: String,
    port: u16,
    /// The request head without the token header, plus any body bytes read
    /// with it.
    forwarded: Vec<u8>,
}

impl PreviewRequest {
    fn parse(buffered: &[u8], header_end: usize, token: &str) -> Result<Self, RelayRefusal> {
        let bad = |reason: &str| RelayRefusal::BadRequest {
            reason: reason.to_string(),
        };
        let head = std::str::from_utf8(&buffered[..header_end - 4])
            .map_err(|_| bad("the request head is not UTF-8"))?;
        let mut lines = head.split("\r\n");
        let request_line = lines.next().ok_or_else(|| bad("no request line"))?;

        let mut presented = None;
        let mut host = None;
        let mut forwarded = Vec::with_capacity(buffered.len());
        forwarded.extend_from_slice(request_line.as_bytes());
        forwarded.extend_from_slice(b"\r\n");
        for line in lines {
            // Folded continuation lines are obsolete (RFC 9112 §5.2) and
            // would let a token hide behind a header the relay keeps.
            if line.starts_with(' ') || line.starts_with('\t') {
                return Err(bad("folded header lines are not accepted"));
            }
            let (name, value) = line
                .split_once(':')
                .ok_or_else(|| bad("a header line has no ':'"))?;
            if name.eq_ignore_ascii_case(WORKER_PREVIEW_RELAY_TOKEN_HEADER) {
                presented = Some(value.trim());
                continue;
            }
            if name.eq_ignore_ascii_case("host") && host.is_none() {
                host = Some(value.trim());
            }
            forwarded.extend_from_slice(line.as_bytes());
            forwarded.extend_from_slice(b"\r\n");
        }
        forwarded.extend_from_slice(b"\r\n");
        forwarded.extend_from_slice(&buffered[header_end..]);

        let authorized = presented
            .map(|presented| constant_time_eq(presented.as_bytes(), token.as_bytes()))
            .unwrap_or(false);
        if !authorized {
            return Err(RelayRefusal::Unauthorized);
        }
        let host = host.ok_or_else(|| bad("no Host header"))?;
        let (hex, port) = parse_preview_host(host).ok_or_else(|| RelayRefusal::BadRequest {
            reason: format!(
                "host '{host}' is not a sandbox preview host (ws-<id>-<port>.<domain>)"
            ),
        })?;
        Ok(Self {
            hex,
            port,
            forwarded,
        })
    }
}

/// `ws-<16 hex>-<port>[.<domain>][:<port>]` → (lowercase hex, port).
fn parse_preview_host(host: &str) -> Option<(String, u16)> {
    let host = host.split(':').next()?;
    let label = host.split('.').next()?;
    let (hex, port) = label.strip_prefix("ws-")?.rsplit_once('-')?;
    let port: u16 = port.parse().ok().filter(|port| *port != 0)?;
    (hex.len() == 16 && hex.chars().all(|c| c.is_ascii_hexdigit()))
        .then(|| (hex.to_ascii_lowercase(), port))
}

/// The channel's failure as the person opening the preview needs it. A
/// refused connection is by far the common case (the dev server is not
/// running, or listens on another port), and socat's raw message buries it
/// under the exec and container layers.
fn explain_channel_failure(port: u16, reason: String) -> String {
    if reason.contains("Connection refused") {
        format!(
            "nothing is listening on port {port} inside the sandbox; start the server \
             there, bound to 0.0.0.0 or 127.0.0.1"
        )
    } else {
        reason
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Read until the end of the request head; returns the index just past it.
async fn read_request_head<R: AsyncRead + Unpin>(
    stream: &mut R,
    buffered: &mut Vec<u8>,
) -> Result<usize, RelayRefusal> {
    let mut chunk = [0u8; 2048];
    loop {
        if let Some(index) = buffered.windows(4).position(|w| w == b"\r\n\r\n") {
            return Ok(index + 4);
        }
        if buffered.len() >= MAX_HEADER_BYTES {
            return Err(RelayRefusal::BadRequest {
                reason: format!("the request head exceeds {MAX_HEADER_BYTES} bytes"),
            });
        }
        let read = stream
            .read(&mut chunk)
            .await
            .map_err(|error| RelayRefusal::BadRequest {
                reason: format!("reading the request failed: {error}"),
            })?;
        if read == 0 {
            return Err(RelayRefusal::BadRequest {
                reason: "the client closed before sending a request head".to_string(),
            });
        }
        buffered.extend_from_slice(&chunk[..read]);
    }
}

/// Send `forwarded` into the channel and splice both directions until the
/// sandbox side ends. `Err` only when the channel failed before any byte
/// reached the client (typically: nothing listens on the port).
async fn splice<C: AsyncRead + AsyncWrite + Unpin>(
    client: &mut C,
    attachment: temps_agents::sandbox::PtyAttachment,
    forwarded: Vec<u8>,
) -> Result<(), String> {
    let temps_agents::sandbox::PtyAttachment {
        mut output,
        mut input,
    } = attachment;
    input
        .write_all(&forwarded)
        .await
        .map_err(|error| format!("sending the request into the sandbox failed: {error}"))?;

    let (mut from_client, mut to_client) = tokio::io::split(client);
    let upload = async move {
        let _ = tokio::io::copy(&mut from_client, &mut input).await;
        let _ = input.shutdown().await;
    };
    let download = async {
        let mut relayed = false;
        while let Some(item) = output.next().await {
            match item {
                Ok(bytes) if bytes.is_empty() => {}
                Ok(bytes) => {
                    if to_client.write_all(&bytes).await.is_err() {
                        return Ok(());
                    }
                    relayed = true;
                }
                Err(error) if !relayed => return Err(error.to_string()),
                Err(error) => {
                    tracing::debug!(%error, "Worker preview channel ended with an error");
                    break;
                }
            }
        }
        let _ = to_client.shutdown().await;
        Ok(())
    };
    tokio::pin!(upload);
    tokio::pin!(download);
    tokio::select! {
        result = &mut download => result,
        () = &mut upload => download.await,
    }
}

async fn write_refusal<W: AsyncWrite + Unpin>(client: &mut W, refusal: &RelayRefusal) {
    let body = format!("{refusal}\n");
    let response = format!(
        "HTTP/1.1 {}\r\ncontent-type: text/plain; charset=utf-8\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
        refusal.status_line(),
        body.len(),
        body
    );
    let _ = client.write_all(response.as_bytes()).await;
    let _ = client.shutdown().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "0123456789abcdef";

    fn head(lines: &[&str]) -> Vec<u8> {
        let mut request = lines.join("\r\n").into_bytes();
        request.extend_from_slice(b"\r\n\r\n");
        request
    }

    fn parse(request: &[u8]) -> Result<PreviewRequest, RelayRefusal> {
        let end = request.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
        PreviewRequest::parse(request, end, TOKEN)
    }

    #[test]
    fn a_tokened_preview_request_is_routed_and_loses_its_token() {
        let mut request = head(&[
            "GET /app HTTP/1.1",
            "Host: ws-00AABBCCDDEEFF11-3000.preview.example.test",
            "X-Temps-Preview-Token: 0123456789abcdef",
            "Accept: */*",
        ]);
        request.extend_from_slice(b"body");
        let parsed = parse(&request).expect("accepted");
        assert_eq!(parsed.hex, "00aabbccddeeff11");
        assert_eq!(parsed.port, 3000);
        let forwarded = String::from_utf8(parsed.forwarded).unwrap();
        assert_eq!(
            forwarded,
            "GET /app HTTP/1.1\r\nHost: ws-00AABBCCDDEEFF11-3000.preview.example.test\r\nAccept: */*\r\n\r\nbody"
        );
    }

    #[test]
    fn a_missing_or_wrong_token_is_refused() {
        let missing = head(&["GET / HTTP/1.1", "Host: ws-00aabbccddeeff11-3000.x"]);
        assert_eq!(parse(&missing), Err(RelayRefusal::Unauthorized));
        let wrong = head(&[
            "GET / HTTP/1.1",
            "Host: ws-00aabbccddeeff11-3000.x",
            "x-temps-preview-token: 0123456789abcdeX",
        ]);
        assert_eq!(parse(&wrong), Err(RelayRefusal::Unauthorized));
    }

    #[test]
    fn folded_headers_and_foreign_hosts_are_bad_requests() {
        let folded = head(&[
            "GET / HTTP/1.1",
            "Host: ws-00aabbccddeeff11-3000.x",
            "X-Other: a",
            " x-temps-preview-token: 0123456789abcdef",
        ]);
        assert!(matches!(
            parse(&folded),
            Err(RelayRefusal::BadRequest { .. })
        ));
        let foreign = head(&[
            "GET / HTTP/1.1",
            "Host: app.example.test",
            "x-temps-preview-token: 0123456789abcdef",
        ]);
        assert!(matches!(
            parse(&foreign),
            Err(RelayRefusal::BadRequest { .. })
        ));
    }

    #[test]
    fn preview_hosts_need_sixteen_hex_digits_and_a_real_port() {
        assert_eq!(
            parse_preview_host("ws-00aabbccddeeff11-8080.d.test:443"),
            Some(("00aabbccddeeff11".to_string(), 8080))
        );
        assert_eq!(parse_preview_host("ws-00aabbccddeeff11-0.d.test"), None);
        assert_eq!(parse_preview_host("ws-00aabbccddeeff1-80.d.test"), None);
        assert_eq!(parse_preview_host("ws-00aabbccddeeff1g-80.d.test"), None);
        assert_eq!(parse_preview_host("app-00aabbccddeeff11-80.d.test"), None);
    }

    fn attachment(
        output: Vec<Result<bytes::Bytes, temps_agents::error::AgentError>>,
    ) -> (
        temps_agents::sandbox::PtyAttachment,
        tokio::io::DuplexStream,
    ) {
        let (input, sandbox_side) = tokio::io::duplex(1024);
        (
            temps_agents::sandbox::PtyAttachment {
                output: Box::pin(futures::stream::iter(output)),
                input: Box::pin(input),
            },
            sandbox_side,
        )
    }

    #[tokio::test]
    async fn splice_forwards_the_request_and_relays_the_answer() {
        let (attachment, mut sandbox_side) = attachment(vec![Ok(bytes::Bytes::from_static(
            b"HTTP/1.1 200 OK\r\n\r\nhi",
        ))]);
        let (mut client, mut user) = tokio::io::duplex(1024);
        splice(&mut client, attachment, b"GET / HTTP/1.1\r\n\r\n".to_vec())
            .await
            .expect("spliced");
        drop(client);
        let mut answer = String::new();
        user.read_to_string(&mut answer).await.unwrap();
        assert_eq!(answer, "HTTP/1.1 200 OK\r\n\r\nhi");
        let mut sent = vec![0u8; 18];
        sandbox_side.read_exact(&mut sent).await.unwrap();
        assert_eq!(&sent, b"GET / HTTP/1.1\r\n\r\n");
    }

    #[tokio::test]
    async fn a_channel_failing_before_any_byte_is_an_error_the_caller_can_answer() {
        let (attachment, _sandbox_side) = attachment(vec![Err(
            temps_agents::error::AgentError::SandboxExecFailed {
                run_id: 0,
                sandbox_id: "x".to_string(),
                reason: "port relay error: Connection refused".to_string(),
            },
        )]);
        let (mut client, _user) = tokio::io::duplex(1024);
        let error = splice(&mut client, attachment, b"GET / HTTP/1.1\r\n\r\n".to_vec())
            .await
            .expect_err("nothing was relayed");
        assert!(error.contains("Connection refused"), "{error}");
    }

    #[test]
    fn a_refused_connection_is_explained_as_nothing_listening() {
        let refused = explain_channel_failure(
            3000,
            "port relay error: socat[104] E connect(5, AF=2 127.0.0.1:3000, 16): Connection refused"
                .to_string(),
        );
        assert!(
            refused.contains("nothing is listening on port 3000"),
            "{refused}"
        );
        assert_eq!(
            explain_channel_failure(3000, "the worker is offline".to_string()),
            "the worker is offline"
        );
    }

    #[tokio::test]
    async fn refusals_are_complete_http_responses() {
        let (mut relay, mut user) = tokio::io::duplex(1024);
        write_refusal(
            &mut relay,
            &RelayRefusal::SandboxNotFound {
                hex: "00aabbccddeeff11".to_string(),
            },
        )
        .await;
        drop(relay);
        let mut answer = String::new();
        user.read_to_string(&mut answer).await.unwrap();
        assert!(answer.starts_with("HTTP/1.1 404 Not Found\r\n"), "{answer}");
        assert!(answer.ends_with("sandbox sbx_00aabbccddeeff11 does not exist or was destroyed\n"));
    }
}
