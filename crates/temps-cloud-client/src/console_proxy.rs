// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! ADR-045 §1–§3: the instance side of the console-proxy tunnel.
//!
//! [`ConsoleProxyWorker`] dials `GET {backend}/v1/console-proxy` as a second,
//! dedicated WebSocket — never layered onto [`crate::heartbeat`]'s connection,
//! for the same head-of-line-blocking reason that one is already split out —
//! negotiates [`Capability::ConsoleProxy`], and then runs the frame state
//! machine defined in [`temps_cloud_protocol::console_proxy`]: it pins the
//! console hostname from the first [`ConsoleOidcConfig`], refuses any
//! [`ConsoleStreamOpen`] that arrives before that pin exists or whose `Host`
//! disagrees with it, and otherwise dispatches the request in-process against
//! whatever [`ConsoleDispatchTarget`] the host process has installed into a
//! [`ConsoleDispatchSlot`] — streaming both the request body in and the
//! response body out, never buffering either whole.
//!
//! # Who owns what
//!
//! This module only ever *uses* a [`ConsoleDispatchTarget`]; it never builds
//! one. The host process (`temps-cli`'s `serve` command) builds the real
//! console `Router` and installs a [`ConsoleRouterHandle`] into a
//! [`ConsoleDispatchSlot`] the moment that router exists — the same "shared
//! slot, installed post-construction" shape
//! `temps-external-plugins::host_api::RouterHostApi` already uses for plugin
//! calls (ADR-045 §3). Likewise this module never decides *whether* to run:
//! the caller (the Cloud plugin, in a later work package) owns the
//! `cloud.console_access_enabled` setting and drives the `enabled` signal
//! passed to [`ConsoleProxyWorker::spawn`]; this module only reacts to it.
//!
//! # What is deliberately out of scope here
//!
//! Provisioning the managed OIDC provider row from a received
//! [`ConsoleOidcConfig`] is not this module's job — it hands the config to a
//! [`ConsoleOidcSink`] the caller supplies (a no-op in every test in this
//! file) and does nothing else with it. Audit logging for a denied Origin or
//! an `InsufficientRole` login likewise belongs to the crate that owns the
//! audit trail (`temps-auth`), not this transport-and-dispatch layer.

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex as StdMutex, RwLock as StdRwLock};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::ConnectInfo;
use axum::http::{HeaderName, HeaderValue, Method, Request, Response, StatusCode};
use futures_util::{
    stream::{SplitSink, SplitStream},
    SinkExt, StreamExt,
};
use hyper_util::rt::TokioIo;
use tokio::sync::{mpsc, watch};
use tokio::task::{AbortHandle, JoinHandle};
// `tokio::time::Instant`, not `std::time::Instant`: it is driven by tokio's
// pausable clock, so `#[tokio::test(start_paused = true)]` can advance it
// deterministically in tests (`idle_clock_survives_periodic_activity_but_fires_after_true_silence`)
// while behaving identically to the standard-library clock in production.
use tokio::time::Instant;
use tokio_tungstenite::{
    tungstenite::{
        client::IntoClientRequest, http::header::AUTHORIZATION, protocol::WebSocketConfig, Message,
    },
    MaybeTlsStream, WebSocketStream,
};
use tower::ServiceExt;
use uuid::Uuid;

use temps_cloud_protocol::{
    Capability, ConsoleDataFrame, ConsoleFrameError, ConsoleFrameKind, ConsoleOidcConfig,
    ConsoleOidcRevoke, ConsoleRefusalReason, ConsoleResponseHead, ConsoleStreamCancel,
    ConsoleStreamEnd, ConsoleStreamEndReason, ConsoleStreamOpen, ConsoleStreamRefused,
    ConsoleWindowUpdate, Envelope, Hello, CONSOLE_DATA_FRAME_HEADER_LEN,
    CONSOLE_MAX_CONCURRENT_STREAMS, CONSOLE_MAX_FRAME_BYTES, CONSOLE_MAX_HEADER_BYTES,
    CONSOLE_STREAM_IDLE_TIMEOUT, CONSOLE_STREAM_WINDOW_BYTES, PROTOCOL_VERSION,
};

use crate::link::CloudLink;
use crate::BackendUrl;

type WsStream = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;
type WsWrite = SplitSink<WsStream, Message>;
type WsRead = SplitStream<WsStream>;

/// Bound on connect + handshake, matching [`crate::heartbeat::HANDSHAKE_TIMEOUT`].
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(8);

/// Total budget for best-effort stream-end notices during teardown.
const STREAM_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

/// Spawned connection drivers must not outlive a cancelled stream or worker.
struct TaskAbortGuard(AbortHandle);

impl Drop for TaskAbortGuard {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Base reconnect delay. Console access is an interactive operator feature —
/// unlike heartbeat, a lost connection should be retried promptly rather than
/// waiting out a 30s cadence built for a liveness signal — but it still backs
/// off, exponentially, to the same ceiling as every other Cloud connection in
/// this crate.
const BASE_RECONNECT_INTERVAL: Duration = Duration::from_secs(5);

/// Outage ceiling for reconnect backoff, matching
/// [`crate::heartbeat::MAX_RECONNECT_INTERVAL`].
const MAX_RECONNECT_INTERVAL: Duration = Duration::from_secs(15 * 60);

/// A handshake alone does not demonstrate backend recovery: repeatedly short
/// connections retain outage backoff until the relay has stayed up this long.
const STABLE_CONNECTION_INTERVAL: Duration = Duration::from_secs(30);

/// Depth of the queue feeding the single writer task. Bounded so a wedged
/// socket applies backpressure to callers rather than growing without limit;
/// sized well above [`CONSOLE_MAX_CONCURRENT_STREAMS`] so ordinary control
/// traffic (refusals, window updates, stream ends) never contends with data
/// frames for room.
const OUTBOUND_QUEUE_CAPACITY: usize = 128;

/// Depth of the channel feeding a request body (or, post-upgrade, the raw
/// relay) into the router. Small on purpose: [`CONSOLE_STREAM_WINDOW_BYTES`]
/// of credit is the real backpressure signal sent to Cloud, and this only
/// needs enough slots that a well-behaved peer never blocks on it.
const STREAM_BODY_CHANNEL_CAPACITY: usize = 8;

/// Transport-level cap on one WebSocket message/frame on the console-proxy
/// connection, enforced by `tokio-tungstenite` itself before this module ever
/// allocates a buffer or attempts JSON/[`ConsoleDataFrame`] decoding —
/// defense in depth underneath [`ConsoleDataFrame::decode`]'s own
/// [`CONSOLE_MAX_FRAME_BYTES`] check, which only runs once a frame has
/// already been read off the wire. Deliberately a small multiple of
/// [`CONSOLE_MAX_FRAME_BYTES`] rather than exactly equal to it:
/// [`CONSOLE_MAX_HEADER_BYTES`] bounds a *logical* `ConsoleStreamOpen`/
/// `ConsoleResponseHead` header list, not the JSON bytes of the
/// `Envelope { kind, payload }` wrapper around it (field names, string
/// escaping), so this budget covers that overhead plus one full data frame
/// of slack.
const WS_TRANSPORT_MAX_MESSAGE_BYTES: usize =
    2 * CONSOLE_MAX_FRAME_BYTES + CONSOLE_MAX_HEADER_BYTES;

/// A synthetic, non-routable loopback peer inserted on every dispatched
/// request so [`temps_core::client_ip::resolve_client_ip`] trusts the
/// `X-Forwarded-For` header this module sets from
/// [`ConsoleStreamOpen::client_ip`] (ADR-045 §3). Never a real socket.
///
/// **Every request this module dispatches carries this loopback
/// `ConnectInfo` by design** — it is not evidence the call arrived over the
/// loopback interface. No admin-router handler may gate behavior on
/// `ConnectInfo::is_loopback()` (or any other direct read of the peer
/// address) to decide trust or admin-only access: doing so would treat every
/// console-proxied browser request as if it originated on the machine
/// itself. [`temps_core::client_ip::resolve_client_ip`] is the one sanctioned
/// consumer of this extension — it is the function that turns this synthetic
/// peer plus the forwarded-for header back into the real browser IP.
const SYNTHETIC_PEER: SocketAddr = SocketAddr::new(std::net::IpAddr::V4(Ipv4Addr::LOCALHOST), 0);

// ---------------------------------------------------------------------------
// Dispatch abstraction — filled by the host process, used by this module
// ---------------------------------------------------------------------------

/// Something that can serve one in-process HTTP request the way the
/// instance's own console `Router` would.
///
/// Implemented by [`ConsoleRouterHandle`] in production; a test may implement
/// it directly against a small `axum::Router` of its own.
#[async_trait::async_trait]
pub trait ConsoleDispatchTarget: Send + Sync {
    /// Serve one request. Must not buffer the response body — the caller
    /// streams it onward frame by frame.
    async fn dispatch(&self, request: Request<Body>) -> Response<Body>;
}

/// Wraps the instance's own console `Router` (admin API + static-file
/// fallback, deliberately without the admin IP-allowlist layer — see
/// ADR-045 §3) so it can be driven as a [`ConsoleDispatchTarget`].
#[derive(Clone)]
pub struct ConsoleRouterHandle {
    router: axum::Router,
}

impl ConsoleRouterHandle {
    pub fn new(router: axum::Router) -> Self {
        Self { router }
    }
}

#[async_trait::async_trait]
impl ConsoleDispatchTarget for ConsoleRouterHandle {
    async fn dispatch(&self, request: Request<Body>) -> Response<Body> {
        match self.router.clone().oneshot(request).await {
            Ok(response) => response,
            // `Router`'s `Service::Error` is `Infallible`.
            Err(never) => match never {},
        }
    }
}

/// A shared slot the host process fills once the real console `Router`
/// exists, and [`ConsoleProxyWorker`] reads on every dispatched request —
/// the same "install post-construction, read per call" shape
/// `temps-external-plugins::host_api::RouterHostApi` already uses.
///
/// Cloning is cheap: it shares the same underlying cell.
#[derive(Clone, Default)]
pub struct ConsoleDispatchSlot(Arc<tokio::sync::RwLock<Option<Arc<dyn ConsoleDispatchTarget>>>>);

impl ConsoleDispatchSlot {
    pub fn new() -> Self {
        Self::default()
    }

    /// Install (or replace) the dispatch target. Idempotent; safe to call
    /// again if the router is ever rebuilt.
    pub async fn set(&self, target: Arc<dyn ConsoleDispatchTarget>) {
        *self.0.write().await = Some(target);
    }

    pub async fn get(&self) -> Option<Arc<dyn ConsoleDispatchTarget>> {
        self.0.read().await.clone()
    }
}

// ---------------------------------------------------------------------------
// OIDC hand-off — filled in later by the Cloud plugin
// ---------------------------------------------------------------------------

/// Receives the managed OIDC provider configuration/revocation this
/// connection carries (ADR-045 §4), without this crate knowing anything
/// about `oidc_providers` rows or `EncryptionService`.
#[async_trait::async_trait]
pub trait ConsoleOidcSink: Send + Sync {
    /// A [`ConsoleOidcConfig`] arrived (once per connection, and again on
    /// every reconnect). The sink is responsible for upserting the managed
    /// provider row idempotently. Routing remains disabled on failure.
    async fn on_config(&self, config: ConsoleOidcConfig) -> Result<(), String>;
    /// Console access was disabled or the link was disconnected from Cloud's
    /// side; run the same teardown the local disable path runs.
    async fn on_revoke(&self);
}

/// Refuses configuration when no OIDC provisioning adapter is installed.
/// An unwired caller must never enable authenticated console routing.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoopConsoleOidcSink;

#[async_trait::async_trait]
impl ConsoleOidcSink for NoopConsoleOidcSink {
    async fn on_config(&self, _config: ConsoleOidcConfig) -> Result<(), String> {
        Err("OIDC provisioning is not configured".into())
    }
    async fn on_revoke(&self) {}
}

// ---------------------------------------------------------------------------
// The worker
// ---------------------------------------------------------------------------

/// Owns nothing by itself — [`Self::spawn`] is the entire public surface.
pub struct ConsoleProxyWorker;

impl ConsoleProxyWorker {
    /// Start the worker on its own task.
    ///
    /// Runs until the returned cancellation sender is sent `true`, gated the
    /// whole time on `enabled` (a snapshot is read at the top of every
    /// connection attempt, and a mid-connection flip to `false` closes the
    /// current connection the same way a shutdown does — see
    /// [`close_all_streams`]) and on [`CloudLink::is_linked`], exactly like
    /// [`crate::heartbeat::run`]. Safe to spawn unconditionally at instance
    /// startup: with `enabled` starting at `false` it does nothing but poll a
    /// `watch` channel until the caller (the Cloud plugin) flips it.
    pub fn spawn(
        link: Arc<CloudLink>,
        enabled: watch::Receiver<bool>,
        dispatch: ConsoleDispatchSlot,
        oidc_sink: Arc<dyn ConsoleOidcSink>,
    ) -> (JoinHandle<()>, watch::Sender<bool>) {
        let (cancel_tx, cancel_rx) = watch::channel(false);
        let handle = tokio::spawn(run(link, enabled, dispatch, oidc_sink, cancel_rx));
        (handle, cancel_tx)
    }
}

async fn run(
    link: Arc<CloudLink>,
    mut enabled: watch::Receiver<bool>,
    dispatch: ConsoleDispatchSlot,
    oidc_sink: Arc<dyn ConsoleOidcSink>,
    mut cancel: watch::Receiver<bool>,
) {
    tracing::info!("Cloud console-proxy worker started");
    let mut retry_in = Duration::ZERO;
    loop {
        tokio::select! {
            biased;
            changed = cancel.changed() => {
                if changed.is_err() || *cancel.borrow() {
                    tracing::info!("Cloud console-proxy worker stopped after shutdown request");
                    return;
                }
            }
            changed = enabled.changed() => {
                if changed.is_err() {
                    return;
                }
                // Explicit operator consent wakes an idle or backed-off worker.
                retry_in = Duration::ZERO;
            }
            _ = tokio::time::sleep(retry_in) => {
                if !link.is_linked() || !*enabled.borrow() {
                    retry_in = BASE_RECONNECT_INTERVAL;
                    continue;
                }
                let outcome = connection_cycle(&link, &dispatch, &oidc_sink, &mut enabled, &mut cancel).await;
                if outcome == CycleOutcome::Cancelled {
                    tracing::info!("Cloud console-proxy worker stopped after shutdown request");
                    return;
                }
                retry_in = next_reconnect_interval(retry_in, outcome);
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CycleOutcome {
    NotLinked,
    Disconnected,
    Connected,
    Cancelled,
}

fn next_reconnect_interval(current: Duration, outcome: CycleOutcome) -> Duration {
    match outcome {
        CycleOutcome::Cancelled => Duration::ZERO,
        CycleOutcome::Connected => BASE_RECONNECT_INTERVAL,
        CycleOutcome::NotLinked | CycleOutcome::Disconnected if current.is_zero() => {
            BASE_RECONNECT_INTERVAL
        }
        CycleOutcome::NotLinked | CycleOutcome::Disconnected => {
            (current * 2).min(MAX_RECONNECT_INTERVAL)
        }
    }
}

async fn connection_cycle(
    link: &Arc<CloudLink>,
    dispatch: &ConsoleDispatchSlot,
    oidc_sink: &Arc<dyn ConsoleOidcSink>,
    enabled: &mut watch::Receiver<bool>,
    cancel: &mut watch::Receiver<bool>,
) -> CycleOutcome {
    let (base_url, token) = match link.linked_credential() {
        Ok(credential) => credential,
        Err(error) => {
            tracing::debug!(%error, "Cloud console-proxy worker has no linked credential");
            return CycleOutcome::NotLinked;
        }
    };
    let backend = match link.parse_backend(&base_url) {
        Ok(backend) => backend,
        Err(error) => {
            tracing::warn!(%error, "Cloud console-proxy worker could not parse the managed backend URL");
            return CycleOutcome::Disconnected;
        }
    };

    let (mut write, mut read) = match connect(&backend, &token).await {
        Ok(streams) => streams,
        Err(error) => {
            tracing::debug!(%error, "Cloud console-proxy connection failed; local operation is unaffected");
            return CycleOutcome::Disconnected;
        }
    };

    let negotiated = match handshake(link, &mut write, &mut read).await {
        Ok(negotiated) => negotiated,
        Err(error) => {
            tracing::warn!(%error, "Cloud console-proxy handshake failed; will retry");
            let _ = write.close().await;
            return CycleOutcome::Disconnected;
        }
    };
    if !negotiated.contains(&Capability::ConsoleProxy) {
        tracing::info!(
            "Cloud backend did not negotiate console-proxy; this Cloud version does not support it yet"
        );
        let _ = write.close().await;
        return CycleOutcome::Disconnected;
    }
    tracing::info!("Cloud console-proxy connection established and negotiated");

    let (outbound_tx, outbound_rx) = mpsc::channel(OUTBOUND_QUEUE_CAPACITY);
    let writer_handle = tokio::spawn(writer_task(write, outbound_rx));
    let _writer_guard = TaskAbortGuard(writer_handle.abort_handle());

    let shared = Arc::new(ConnectionShared {
        console_host: StdRwLock::new(None),
        streams: StdMutex::new(HashMap::new()),
        outbound: outbound_tx.clone(),
        dispatch: dispatch.clone(),
    });

    let connected_at = Instant::now();
    let outcome = read_loop(&shared, &mut read, oidc_sink, enabled, cancel).await;
    let outcome = connection_outcome(outcome, connected_at.elapsed());

    // ADR-045 §1: announce every open stream before the socket closes, so
    // Cloud can offer the browser a retry instead of a silent hard cut. Best
    // effort and time-boxed by the surrounding logic (a hung writer task is
    // bounded by `writer_handle`'s own timeout below), never a reason to
    // delay the shutdown or reconnect this announces.
    let reason = if outcome == CycleOutcome::Cancelled {
        ConsoleStreamEndReason::GoingAway
    } else {
        // A disabled setting or a lost connection: the same "come back later"
        // signal from the browser's point of view. See the module docs for
        // why one reason covers both cases.
        ConsoleStreamEndReason::GoingAway
    };
    close_all_streams(&shared, reason).await;
    drop(outbound_tx);
    drop(shared);
    let _ = tokio::time::timeout(Duration::from_secs(5), writer_handle).await;

    outcome
}

fn connection_outcome(outcome: CycleOutcome, connected_for: Duration) -> CycleOutcome {
    if outcome == CycleOutcome::Disconnected && connected_for >= STABLE_CONNECTION_INTERVAL {
        CycleOutcome::Connected
    } else {
        outcome
    }
}

async fn connect(
    backend: &BackendUrl,
    token: &str,
) -> Result<(WsWrite, WsRead), ConsoleProxyError> {
    let url = console_proxy_ws_url(backend)?;
    let mut request =
        url.as_str()
            .into_client_request()
            .map_err(|error| ConsoleProxyError::InvalidRequest {
                reason: error.to_string(),
            })?;
    let authorization = format!("Bearer {token}")
        .parse::<tokio_tungstenite::tungstenite::http::HeaderValue>()
        .map_err(|error| ConsoleProxyError::InvalidRequest {
            reason: error.to_string(),
        })?;
    request.headers_mut().insert(AUTHORIZATION, authorization);

    // Reject an oversize control envelope or data frame at the transport
    // layer, before any allocation or decoding — see
    // `WS_TRANSPORT_MAX_MESSAGE_BYTES`'s own doc comment.
    let config = WebSocketConfig::default()
        .max_message_size(Some(WS_TRANSPORT_MAX_MESSAGE_BYTES))
        .max_frame_size(Some(WS_TRANSPORT_MAX_MESSAGE_BYTES));
    let (stream, _response) = tokio::time::timeout(
        HANDSHAKE_TIMEOUT,
        tokio_tungstenite::connect_async_with_config(request, Some(config), false),
    )
    .await
    .map_err(|_| ConsoleProxyError::ConnectTimeout)?
    .map_err(|source| ConsoleProxyError::Connect { source })?;
    let (write, read) = stream.split();
    Ok((write, read))
}

/// `wss://{host}/v1/console-proxy`, or `ws://` for an explicit loopback
/// development backend, mirroring [`crate::heartbeat::management_ws_url`].
fn console_proxy_ws_url(backend: &BackendUrl) -> Result<url::Url, ConsoleProxyError> {
    let mut url = backend.endpoint("/v1/console-proxy");
    let scheme = match url.scheme() {
        "https" => "wss",
        "http" => "ws",
        other => {
            return Err(ConsoleProxyError::InvalidRequest {
                reason: format!("unsupported scheme {other:?} for the console-proxy channel"),
            })
        }
    };
    url.set_scheme(scheme)
        .map_err(|()| ConsoleProxyError::InvalidRequest {
            reason: format!("could not switch the console-proxy endpoint to the {scheme} scheme"),
        })?;
    Ok(url)
}

async fn handshake(
    link: &CloudLink,
    write: &mut WsWrite,
    read: &mut WsRead,
) -> Result<Vec<Capability>, ConsoleProxyError> {
    let server_hello = tokio::time::timeout(HANDSHAKE_TIMEOUT, read.next())
        .await
        .map_err(|_| ConsoleProxyError::HelloTimeout)?
        .ok_or(ConsoleProxyError::ConnectionClosedDuringHandshake)?
        .map_err(|source| ConsoleProxyError::Connect { source })
        .and_then(|message| decode_hello(&message))?;

    let our_capabilities = vec![Capability::ConsoleProxy];
    let hello = Hello {
        protocol_version: PROTOCOL_VERSION,
        agent_version: link.agent_version().to_string(),
        capabilities: our_capabilities.clone(),
    };
    send_envelope_now(write, "hello", &hello).await?;

    Ok(our_capabilities
        .into_iter()
        .filter(|capability| server_hello.capabilities.contains(capability))
        .collect())
}

async fn send_envelope_now<T: serde::Serialize>(
    write: &mut WsWrite,
    kind: &str,
    payload: &T,
) -> Result<(), ConsoleProxyError> {
    let envelope =
        Envelope::new(kind, payload).map_err(|source| ConsoleProxyError::Encode { source })?;
    let text =
        serde_json::to_string(&envelope).map_err(|source| ConsoleProxyError::Encode { source })?;
    write
        .send(Message::Text(text.into()))
        .await
        .map_err(|source| ConsoleProxyError::Send { source })
}

fn decode_envelope(message: &Message) -> Option<Envelope> {
    let text = message.to_text().ok()?;
    serde_json::from_str(text).ok()
}

fn decode_hello(message: &Message) -> Result<Hello, ConsoleProxyError> {
    let envelope = decode_envelope(message).ok_or(ConsoleProxyError::InvalidServerHello {
        reason: "the first frame was not a readable envelope".to_string(),
    })?;
    let kind = envelope.kind.clone();
    envelope
        .decode::<Hello>("hello")
        .ok_or(ConsoleProxyError::InvalidServerHello {
            reason: format!("expected a hello envelope, got kind {kind:?}"),
        })
}

fn is_close(message: &Message) -> bool {
    matches!(message, Message::Close(_))
}

// ---------------------------------------------------------------------------
// Idle (not absolute) per-stream timeout
// ---------------------------------------------------------------------------

/// The last moment a stream did anything, so
/// [`CONSOLE_STREAM_IDLE_TIMEOUT`] can be enforced as a true inactivity
/// timer rather than a hard cap on total stream duration.
///
/// ADR-045 explicitly calls an unbounded log tail "the normal case, not an
/// edge case to reject" — a single `tokio::time::timeout` wrapped around a
/// whole stream would kill exactly that case after 120s regardless of how
/// much real traffic kept flowing, so every activity-producing event
/// (inbound data/window frame, outbound chunk/relay write) must push this
/// clock forward instead.
struct ActivityClock {
    last: StdMutex<Instant>,
}

impl ActivityClock {
    fn new() -> Self {
        Self {
            last: StdMutex::new(Instant::now()),
        }
    }

    fn touch(&self) {
        *self.last.lock().unwrap_or_else(|p| p.into_inner()) = Instant::now();
    }

    fn elapsed(&self) -> Duration {
        self.last
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .elapsed()
    }
}

/// Resolves once `clock` has gone `timeout` without a [`ActivityClock::touch`],
/// re-checking after every partial sleep so a touch that lands while this is
/// asleep correctly pushes the deadline back rather than firing on stale
/// state.
async fn idle_watchdog(clock: &ActivityClock, timeout: Duration) {
    loop {
        let elapsed = clock.elapsed();
        let Some(remaining) = timeout.checked_sub(elapsed) else {
            return;
        };
        if remaining.is_zero() {
            return;
        }
        tokio::time::sleep(remaining).await;
    }
}

// ---------------------------------------------------------------------------
// Per-connection shared state
// ---------------------------------------------------------------------------

struct StreamEntry {
    /// Where the next inbound data frame's payload goes: the request body
    /// channel before a response is sent, or the WebSocket relay-write
    /// channel afterward (swapped exactly once, on a successful upgrade).
    /// `None` means "no body expected" (a GET with no declared
    /// `Content-Length`) — an inbound data frame arriving in that state is a
    /// protocol violation from Cloud and is logged, not delivered anywhere.
    inbound: Arc<StdMutex<Option<mpsc::Sender<Bytes>>>>,
    /// Bytes still expected on `inbound` before it is closed (dropped) to
    /// signal end-of-body to the router. `< 0` means "unbounded / not
    /// tracked" (the post-upgrade relay phase, or no body at all).
    remaining_inbound_bytes: Arc<AtomicI64>,
    /// Send-side credit Cloud has granted this stream for
    /// `ResponseBodyChunk`/`WsRelay` frames. Replenished by
    /// [`ConsoleWindowUpdate`] frames from Cloud.
    outbound_credit: Arc<tokio::sync::Semaphore>,
    /// Ticks forward on every inbound and outbound frame for this stream —
    /// see [`ActivityClock`] and [`idle_watchdog`].
    activity: Arc<ActivityClock>,
    /// Aborts the stream's task immediately — used both for an explicit
    /// [`ConsoleStreamCancel`] and for connection-wide shutdown.
    abort: AbortHandle,
}

struct ConnectionShared {
    /// The pinned console hostname from the most recent [`ConsoleOidcConfig`].
    /// `None` until the first one arrives on this connection — the fail-closed
    /// state ADR-045 §3 requires.
    console_host: StdRwLock<Option<String>>,
    streams: StdMutex<HashMap<Uuid, StreamEntry>>,
    outbound: mpsc::Sender<Message>,
    dispatch: ConsoleDispatchSlot,
}

impl ConnectionShared {
    async fn send_message(&self, message: Message) -> Result<(), ConsoleProxyError> {
        self.outbound
            .send(message)
            .await
            .map_err(|_| ConsoleProxyError::ConnectionClosed)
    }

    async fn send_envelope<T: serde::Serialize>(
        &self,
        kind: &str,
        payload: &T,
    ) -> Result<(), ConsoleProxyError> {
        let envelope =
            Envelope::new(kind, payload).map_err(|source| ConsoleProxyError::Encode { source })?;
        let text = serde_json::to_string(&envelope)
            .map_err(|source| ConsoleProxyError::Encode { source })?;
        self.send_message(Message::Text(text.into())).await
    }

    fn pinned_host(&self) -> Option<String> {
        self.console_host
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }
}

async fn writer_task(mut write: WsWrite, mut rx: mpsc::Receiver<Message>) {
    while let Some(message) = rx.recv().await {
        if write.send(message).await.is_err() {
            break;
        }
    }
    let _ = write.close().await;
}

/// Refuse a stream outright — the router is never called. See
/// [`ConsoleRefusalReason`]'s own doc comment for what each variant means.
async fn refuse_stream(shared: &ConnectionShared, stream_id: Uuid, reason: ConsoleRefusalReason) {
    tracing::debug!(%stream_id, ?reason, "refusing console-proxy stream");
    let refused = ConsoleStreamRefused { stream_id, reason };
    let _ = shared
        .send_envelope(ConsoleStreamRefused::KIND, &refused)
        .await;
}

/// Claim-and-end a stream: only the caller that successfully removes the
/// table entry sends the [`ConsoleStreamEnd`] frame, so a race between the
/// stream's own task finishing and a connection-wide shutdown can never send
/// two end frames for the same stream.
async fn end_stream(shared: &ConnectionShared, stream_id: Uuid, reason: ConsoleStreamEndReason) {
    let removed = shared
        .streams
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .remove(&stream_id);
    if removed.is_some() {
        let end = ConsoleStreamEnd { stream_id, reason };
        let _ = shared.send_envelope(ConsoleStreamEnd::KIND, &end).await;
    }
}

/// ADR-045 §1: announce every currently open stream before the socket closes.
async fn close_all_streams(shared: &ConnectionShared, reason: ConsoleStreamEndReason) {
    let entries: Vec<(Uuid, StreamEntry)> = shared
        .streams
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .drain()
        .collect();
    // Abort all tasks before attempting any potentially blocked writes.
    for (_, entry) in &entries {
        entry.abort.abort();
    }
    let notices = async {
        for (stream_id, _) in entries {
            let end = ConsoleStreamEnd {
                stream_id,
                reason: reason.clone(),
            };
            let _ = shared.send_envelope(ConsoleStreamEnd::KIND, &end).await;
        }
    };
    if tokio::time::timeout(STREAM_SHUTDOWN_TIMEOUT, notices)
        .await
        .is_err()
    {
        tracing::warn!(
            "console-proxy stream-end notices timed out; all streams are already aborted"
        );
    }
}

fn header_value<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

// ---------------------------------------------------------------------------
// Read loop
// ---------------------------------------------------------------------------

async fn read_loop(
    shared: &Arc<ConnectionShared>,
    read: &mut WsRead,
    oidc_sink: &Arc<dyn ConsoleOidcSink>,
    enabled: &mut watch::Receiver<bool>,
    cancel: &mut watch::Receiver<bool>,
) -> CycleOutcome {
    loop {
        tokio::select! {
            biased;
            changed = cancel.changed() => {
                if changed.is_err() || *cancel.borrow() {
                    return CycleOutcome::Cancelled;
                }
            }
            changed = enabled.changed() => {
                if changed.is_err() || !*enabled.borrow() {
                    tracing::info!("Cloud console access disabled; closing the console-proxy connection");
                    return CycleOutcome::Disconnected;
                }
            }
            message = read.next() => {
                match message {
                    Some(Ok(message)) if is_close(&message) => {
                        tracing::debug!("Cloud console-proxy connection closed by the backend");
                        return CycleOutcome::Disconnected;
                    }
                    Some(Ok(message)) => {
                        let is_enabled = *enabled.borrow();
                        let handling = handle_message(shared, message, oidc_sink, is_enabled);
                        tokio::pin!(handling);
                        // Body backpressure or provisioning must never prevent an
                        // operator from closing this connection. Keep the same
                        // future across unrelated watch notifications.
                        loop {
                            tokio::select! {
                                biased;
                                changed = cancel.changed() => {
                                    if changed.is_err() || *cancel.borrow() {
                                        return CycleOutcome::Cancelled;
                                    }
                                }
                                changed = enabled.changed() => {
                                    if changed.is_err() || !*enabled.borrow() {
                                        return CycleOutcome::Disconnected;
                                    }
                                }
                                _ = &mut handling => break,
                            }
                        }
                    }
                    Some(Err(error)) => {
                        tracing::debug!(%error, "Cloud console-proxy connection error; will reconnect");
                        return CycleOutcome::Disconnected;
                    }
                    None => {
                        tracing::debug!("Cloud console-proxy connection closed");
                        return CycleOutcome::Disconnected;
                    }
                }
            }
        }
    }
}

async fn handle_message(
    shared: &Arc<ConnectionShared>,
    message: Message,
    oidc_sink: &Arc<dyn ConsoleOidcSink>,
    enabled: bool,
) {
    match message {
        Message::Binary(bytes) => match ConsoleDataFrame::decode(&bytes) {
            Ok(frame) => route_data_frame(shared, frame).await,
            Err(error) => {
                tracing::warn!(%error, "dropping an unreadable console-proxy data frame");
            }
        },
        Message::Text(_) => {
            let Some(envelope) = decode_envelope(&message) else {
                return;
            };
            handle_envelope(shared, envelope, oidc_sink, enabled).await;
        }
        _ => {}
    }
}

async fn handle_envelope(
    shared: &Arc<ConnectionShared>,
    envelope: Envelope,
    oidc_sink: &Arc<dyn ConsoleOidcSink>,
    enabled: bool,
) {
    if let Some(config) = envelope.decode::<ConsoleOidcConfig>(ConsoleOidcConfig::KIND) {
        // Provisioning must succeed before routing browser requests. A
        // conflicting issuer or failed database write must fail closed.
        *shared
            .console_host
            .write()
            .unwrap_or_else(|p| p.into_inner()) = None;
        close_all_streams(shared, ConsoleStreamEndReason::GoingAway).await;
        let host = config.console_host.clone();
        match oidc_sink.on_config(config).await {
            Ok(()) => {
                *shared
                    .console_host
                    .write()
                    .unwrap_or_else(|p| p.into_inner()) = Some(host);
            }
            Err(error) => {
                tracing::error!(%error, "console-proxy OIDC provisioning failed; routing remains disabled")
            }
        }
        return;
    }
    if envelope
        .decode::<ConsoleOidcRevoke>(ConsoleOidcRevoke::KIND)
        .is_some()
    {
        tracing::info!("console-proxy OIDC revoke received");
        *shared
            .console_host
            .write()
            .unwrap_or_else(|p| p.into_inner()) = None;
        close_all_streams(shared, ConsoleStreamEndReason::GoingAway).await;
        oidc_sink.on_revoke().await;
        return;
    }
    if let Some(open) = envelope.decode::<ConsoleStreamOpen>(ConsoleStreamOpen::KIND) {
        handle_stream_open(shared.clone(), open, enabled).await;
        return;
    }
    if let Some(cancel) = envelope.decode::<ConsoleStreamCancel>(ConsoleStreamCancel::KIND) {
        let entry = shared
            .streams
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&cancel.stream_id);
        if let Some(entry) = entry {
            entry.abort.abort();
        }
        return;
    }
    if let Some(update) = envelope.decode::<ConsoleWindowUpdate>(ConsoleWindowUpdate::KIND) {
        let table = shared.streams.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(entry) = table.get(&update.stream_id) {
            entry
                .outbound_credit
                .add_permits(update.additional_bytes as usize);
            // A window update is Cloud acknowledging outbound bytes it
            // received — inbound activity for idle-timeout purposes.
            entry.activity.touch();
        }
    }
    // An unrecognized kind (including our own `hello`/`heartbeat`-shaped
    // frames a misconfigured peer might echo) is dropped, never fatal — the
    // crate-wide forward-compatibility rule.
}

async fn route_data_frame(shared: &Arc<ConnectionShared>, frame: ConsoleDataFrame) {
    if matches!(frame.frame_kind, ConsoleFrameKind::ResponseBodyChunk) {
        tracing::warn!(
            stream_id = %frame.stream_id,
            "received a response-body chunk, which only ever flows instance→Cloud; ignoring"
        );
        return;
    }
    let sender = {
        let table = shared.streams.lock().unwrap_or_else(|p| p.into_inner());
        table.get(&frame.stream_id).map(|entry| {
            (
                entry.inbound.clone(),
                entry.remaining_inbound_bytes.clone(),
                entry.activity.clone(),
            )
        })
    };
    let Some((inbound, remaining, activity)) = sender else {
        // Frame for a stream that was refused, cancelled, or already ended.
        return;
    };
    let payload_len = frame.payload().len();
    let current_sender = { inbound.lock().unwrap_or_else(|p| p.into_inner()).clone() };
    let Some(current_sender) = current_sender else {
        tracing::debug!(
            stream_id = %frame.stream_id,
            "received a body chunk for a stream with no open inbound channel; ignoring"
        );
        return;
    };
    if current_sender.send(frame.payload().clone()).await.is_err() {
        return;
    }
    activity.touch();
    // Replenish exactly what was consumed. Deliberately not batched: the
    // handful of concurrent operator streams this connection carries makes
    // the extra JSON frames immaterial, and always replenishing exactly the
    // consumed amount is simpler to reason about and to test than a
    // threshold-based scheme — see the module docs.
    let update = ConsoleWindowUpdate {
        stream_id: frame.stream_id,
        additional_bytes: payload_len as u32,
    };
    let _ = shared
        .send_envelope(ConsoleWindowUpdate::KIND, &update)
        .await;

    if remaining.load(Ordering::Acquire) >= 0 {
        let left = remaining.fetch_sub(payload_len as i64, Ordering::AcqRel) - payload_len as i64;
        if left <= 0 {
            // Full declared body received: close the channel so the router
            // sees end-of-body rather than hanging on more bytes that will
            // never come.
            inbound.lock().unwrap_or_else(|p| p.into_inner()).take();
        }
    }
}

// ---------------------------------------------------------------------------
// Opening and running one stream
// ---------------------------------------------------------------------------

async fn handle_stream_open(shared: Arc<ConnectionShared>, open: ConsoleStreamOpen, enabled: bool) {
    let stream_id = open.stream_id;

    if !enabled {
        refuse_stream(&shared, stream_id, ConsoleRefusalReason::Disabled).await;
        return;
    }
    let Some(pinned_host) = shared.pinned_host() else {
        refuse_stream(&shared, stream_id, ConsoleRefusalReason::NotConfigured).await;
        return;
    };
    if open
        .client_ip
        .as_deref()
        .and_then(|ip| ip.parse::<std::net::IpAddr>().ok())
        .is_none()
    {
        refuse_stream(&shared, stream_id, ConsoleRefusalReason::NotConfigured).await;
        return;
    }
    let declared_host = header_value(&open.headers, "host");
    if declared_host != Some(pinned_host.as_str()) {
        refuse_stream(&shared, stream_id, ConsoleRefusalReason::HostMismatch).await;
        return;
    }
    // A reused `stream_id` must never overwrite the existing table entry —
    // that would desync this connection's bookkeeping from Cloud's and
    // silently drop whatever the original stream was doing. Checked before
    // the concurrency limit so a duplicate is always reported as exactly
    // that, never conflated with `TooManyStreams`.
    let is_duplicate = shared
        .streams
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .contains_key(&stream_id);
    if is_duplicate {
        refuse_stream(&shared, stream_id, ConsoleRefusalReason::DuplicateStream).await;
        return;
    }
    let header_bytes: usize = open.headers.iter().map(|(k, v)| k.len() + v.len()).sum();
    if header_bytes > CONSOLE_MAX_HEADER_BYTES {
        let end = ConsoleStreamEnd {
            stream_id,
            reason: ConsoleStreamEndReason::Error {
                detail: format!("request headers total {header_bytes} bytes, over the {CONSOLE_MAX_HEADER_BYTES}-byte limit"),
            },
        };
        // No table entry exists yet, so end_stream would silently do nothing.
        let _ = shared.send_envelope(ConsoleStreamEnd::KIND, &end).await;
        return;
    }
    let too_many_streams = shared
        .streams
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .len()
        >= CONSOLE_MAX_CONCURRENT_STREAMS;
    if too_many_streams {
        refuse_stream(&shared, stream_id, ConsoleRefusalReason::TooManyStreams).await;
        return;
    }
    if open.upgrade_requested {
        let origin = header_value(&open.headers, "origin");
        let expected_origin = format!("https://{pinned_host}");
        if origin != Some(expected_origin.as_str()) {
            refuse_stream(&shared, stream_id, ConsoleRefusalReason::OriginMismatch).await;
            return;
        }
    }
    let Some(target) = shared.dispatch.get().await else {
        // Nothing to dispatch to yet — the host process has not installed a
        // router into the slot. From Cloud's point of view this is
        // indistinguishable from "not configured": there is nothing to
        // compare or serve.
        refuse_stream(&shared, stream_id, ConsoleRefusalReason::NotConfigured).await;
        return;
    };

    let content_length = header_value(&open.headers, "content-length").and_then(|value| {
        let parsed: u64 = value.trim().parse().ok()?;
        (parsed > 0).then_some(parsed)
    });
    let (inbound_tx, inbound_rx) = if content_length.is_some() && !open.upgrade_requested {
        let (tx, rx) = mpsc::channel(STREAM_BODY_CHANNEL_CAPACITY);
        (Some(tx), Some(rx))
    } else {
        (None, None)
    };

    let inbound = Arc::new(StdMutex::new(inbound_tx));
    let remaining_inbound_bytes = Arc::new(AtomicI64::new(
        content_length.map(|n| n as i64).unwrap_or(-1),
    ));
    let outbound_credit = Arc::new(tokio::sync::Semaphore::new(
        CONSOLE_STREAM_WINDOW_BYTES as usize,
    ));
    let activity = Arc::new(ActivityClock::new());

    let stream_shared = shared.clone();
    let inbound_for_task = inbound.clone();
    let remaining_for_task = remaining_inbound_bytes.clone();
    let credit_for_task = outbound_credit.clone();
    let activity_for_task = activity.clone();
    let (registered_tx, registered_rx) = tokio::sync::oneshot::channel();
    let join = tokio::spawn(async move {
        if registered_rx.await.is_err() {
            return;
        }
        run_stream(
            stream_shared,
            open,
            pinned_host,
            target,
            inbound_for_task,
            inbound_rx,
            remaining_for_task,
            credit_for_task,
            activity_for_task,
        )
        .await
    });

    shared
        .streams
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(
            stream_id,
            StreamEntry {
                inbound,
                remaining_inbound_bytes,
                outbound_credit,
                activity,
                abort: join.abort_handle(),
            },
        );
    let _ = registered_tx.send(());
}

#[allow(clippy::too_many_arguments)]
async fn run_stream(
    shared: Arc<ConnectionShared>,
    open: ConsoleStreamOpen,
    pinned_host: String,
    target: Arc<dyn ConsoleDispatchTarget>,
    inbound: Arc<StdMutex<Option<mpsc::Sender<Bytes>>>>,
    inbound_rx: Option<mpsc::Receiver<Bytes>>,
    remaining_inbound_bytes: Arc<AtomicI64>,
    outbound_credit: Arc<tokio::sync::Semaphore>,
    activity: Arc<ActivityClock>,
) {
    let stream_id = open.stream_id;
    let work = async {
        if open.upgrade_requested {
            run_upgrade_stream(
                &shared,
                &open,
                &pinned_host,
                target.clone(),
                &outbound_credit,
                &activity,
            )
            .await
        } else {
            run_normal_stream(
                &shared,
                &open,
                &pinned_host,
                target.clone(),
                inbound_rx,
                &outbound_credit,
                &activity,
            )
            .await
        }
    };
    tokio::pin!(work);

    // A true inactivity timer, not a hard cap on total stream duration
    // (ADR-045 §2: unbounded log tails are the normal case) — every inbound
    // and outbound frame for this stream pushes `activity` forward via
    // `route_data_frame`, the `ConsoleWindowUpdate` handler, and
    // `send_credited`, so `idle_watchdog` only fires after a real silence of
    // `CONSOLE_STREAM_IDLE_TIMEOUT`, however long the stream has been open in
    // total.
    let outcome = tokio::select! {
        result = &mut work => Ok(result),
        () = idle_watchdog(&activity, CONSOLE_STREAM_IDLE_TIMEOUT) => Err(()),
    };

    // Whatever channel remained open (unconsumed body, unfinished relay) is
    // dropped along with `inbound`/`remaining_inbound_bytes` here; nothing
    // else references them once the stream's own task ends.
    let _ = remaining_inbound_bytes;
    let _ = inbound;

    match outcome {
        Ok(Ok(())) => end_stream(&shared, stream_id, ConsoleStreamEndReason::Complete).await,
        Ok(Err(error)) => {
            end_stream(
                &shared,
                stream_id,
                ConsoleStreamEndReason::Error {
                    detail: error.to_string(),
                },
            )
            .await
        }
        Err(()) => {
            end_stream(
                &shared,
                stream_id,
                ConsoleStreamEndReason::Error {
                    detail: format!(
                        "stream idle for over {}s",
                        CONSOLE_STREAM_IDLE_TIMEOUT.as_secs()
                    ),
                },
            )
            .await
        }
    }
}

fn build_request(
    open: &ConsoleStreamOpen,
    pinned_host: &str,
    body: Body,
) -> Result<Request<Body>, ConsoleProxyError> {
    let method = Method::from_bytes(open.method.as_bytes()).map_err(|error| {
        ConsoleProxyError::InvalidStreamRequest {
            stream_id: open.stream_id,
            reason: format!("invalid method {:?}: {error}", open.method),
        }
    })?;
    let mut uri = open.path.clone();
    if let Some(query) = &open.query {
        if !query.is_empty() {
            uri.push('?');
            uri.push_str(query);
        }
    }
    let mut builder = Request::builder().method(method).uri(uri);
    for (name, value) in &open.headers {
        // Forwarding metadata comes only from the relay's validated client IP
        // and pinned host, never from headers supplied by a browser.
        if name.eq_ignore_ascii_case("host")
            || name.eq_ignore_ascii_case("forwarded")
            || name.eq_ignore_ascii_case("x-real-ip")
            || name.to_ascii_lowercase().starts_with("x-forwarded-")
            || (is_hop_by_hop_header(name, &open.headers)
                && !(open.upgrade_requested
                    && (name.eq_ignore_ascii_case("connection")
                        || name.eq_ignore_ascii_case("upgrade"))))
        {
            continue;
        }
        let header_name = HeaderName::from_bytes(name.as_bytes()).map_err(|error| {
            ConsoleProxyError::InvalidStreamRequest {
                stream_id: open.stream_id,
                reason: format!("invalid header name {name:?}: {error}"),
            }
        })?;
        let header_value = HeaderValue::from_str(value).map_err(|error| {
            ConsoleProxyError::InvalidStreamRequest {
                stream_id: open.stream_id,
                reason: format!("invalid header value for {name:?}: {error}"),
            }
        })?;
        builder = builder.header(header_name, header_value);
    }
    builder = builder
        .header(axum::http::header::HOST, pinned_host)
        .header("x-forwarded-proto", "https");
    if let Some(client_ip) = &open.client_ip {
        if let Ok(value) = HeaderValue::from_str(client_ip) {
            builder = builder.header("x-forwarded-for", value);
        }
    }
    let mut request =
        builder
            .body(body)
            .map_err(|error| ConsoleProxyError::InvalidStreamRequest {
                stream_id: open.stream_id,
                reason: error.to_string(),
            })?;
    // ADR-045 §3: the load-bearing detail that makes
    // `temps_core::client_ip::resolve_client_ip` trust the
    // `X-Forwarded-For` header set above instead of collapsing every
    // console-proxied request's audited IP to "unknown".
    request.extensions_mut().insert(ConnectInfo(SYNTHETIC_PEER));
    Ok(request)
}

fn is_hop_by_hop_header(name: &str, headers: &[(String, String)]) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
    ) || headers
        .iter()
        .filter(|(key, _)| key.eq_ignore_ascii_case("connection"))
        .any(|(_, value)| {
            value
                .split(',')
                .any(|token| token.trim().eq_ignore_ascii_case(name))
        })
}

fn max_data_payload_bytes() -> usize {
    CONSOLE_MAX_FRAME_BYTES - CONSOLE_DATA_FRAME_HEADER_LEN
}

/// Send `data` to Cloud as one or more [`ConsoleDataFrame`]s of `kind`,
/// respecting both [`CONSOLE_MAX_FRAME_BYTES`] and the stream's outbound
/// credit. Blocks (this stream only) while credit is exhausted — the
/// documented, deliberate difference from the hot-path "drop" rule: dropping
/// HTTP body bytes corrupts the response.
async fn send_credited(
    shared: &ConnectionShared,
    stream_id: Uuid,
    kind: ConsoleFrameKind,
    credit: &tokio::sync::Semaphore,
    activity: &ActivityClock,
    mut data: Bytes,
) -> Result<(), ConsoleProxyError> {
    let max_payload = max_data_payload_bytes();
    while !data.is_empty() {
        let take = data.len().min(max_payload);
        let chunk = data.split_to(take);
        let permit = credit
            .acquire_many(take as u32)
            .await
            .map_err(|_| ConsoleProxyError::ConnectionClosed)?;
        permit.forget();
        let frame = ConsoleDataFrame::new(kind, stream_id, chunk)
            .map_err(|source| ConsoleProxyError::Frame { source })?;
        shared.send_message(Message::Binary(frame.encode())).await?;
        // Every outbound chunk/relay write is activity for idle-timeout
        // purposes, matching the inbound touches in `route_data_frame` and
        // the `ConsoleWindowUpdate` handler.
        activity.touch();
    }
    Ok(())
}

/// Convert a router response into a [`ConsoleResponseHead`] plus zero or more
/// data frames, never buffering the body whole.
#[allow(clippy::too_many_arguments)]
async fn stream_response(
    shared: &ConnectionShared,
    stream_id: Uuid,
    kind: ConsoleFrameKind,
    credit: &tokio::sync::Semaphore,
    activity: &ActivityClock,
    response: Response<Body>,
    upgraded: bool,
) -> Result<(), ConsoleProxyError> {
    let status = response.status();
    // Check the original header map before copying any unbounded values.
    validate_response_headers(stream_id, response.headers())?;
    let connection_headers: Vec<(String, String)> = response
        .headers()
        .get_all("connection")
        .iter()
        .filter_map(|value| {
            value
                .to_str()
                .ok()
                .map(|value| ("connection".into(), value.into()))
        })
        .collect();
    let headers: Vec<(String, String)> = response
        .headers()
        .iter()
        .filter_map(|(name, value)| {
            // Hop-by-hop framing headers are meaningless once re-framed as
            // console-proxy data frames.
            if is_hop_by_hop_header(name.as_str(), &connection_headers) {
                return None;
            }
            value
                .to_str()
                .ok()
                .map(|v| (name.to_string(), v.to_string()))
        })
        .collect();
    let head = ConsoleResponseHead {
        stream_id,
        status: status.as_u16(),
        headers,
        upgraded,
    };
    shared
        .send_envelope(ConsoleResponseHead::KIND, &head)
        .await?;

    let mut body = response.into_body().into_data_stream();
    while let Some(chunk) = body.next().await {
        let chunk = chunk.map_err(|source| ConsoleProxyError::ResponseBody {
            stream_id,
            reason: source.to_string(),
        })?;
        if chunk.is_empty() {
            continue;
        }
        send_credited(shared, stream_id, kind, credit, activity, chunk).await?;
    }
    Ok(())
}

fn validate_response_headers(
    stream_id: Uuid,
    headers: &axum::http::HeaderMap,
) -> Result<(), ConsoleProxyError> {
    let bytes: usize = headers
        .iter()
        .map(|(name, value)| name.as_str().len() + value.as_bytes().len())
        .sum();
    if bytes > CONSOLE_MAX_HEADER_BYTES {
        return Err(ConsoleProxyError::InvalidStreamResponse {
            stream_id,
            reason: format!("response headers total {bytes} bytes, over the {CONSOLE_MAX_HEADER_BYTES}-byte limit"),
        });
    }
    Ok(())
}

async fn run_normal_stream(
    shared: &Arc<ConnectionShared>,
    open: &ConsoleStreamOpen,
    pinned_host: &str,
    target: Arc<dyn ConsoleDispatchTarget>,
    inbound_rx: Option<mpsc::Receiver<Bytes>>,
    outbound_credit: &tokio::sync::Semaphore,
    activity: &ActivityClock,
) -> Result<(), ConsoleProxyError> {
    let body = match inbound_rx {
        Some(rx) => Body::from_stream(receiver_stream(rx)),
        None => Body::empty(),
    };
    let request = build_request(open, pinned_host, body)?;
    let response = target.dispatch(request).await;
    stream_response(
        shared,
        open.stream_id,
        ConsoleFrameKind::ResponseBodyChunk,
        outbound_credit,
        activity,
        response,
        false,
    )
    .await
}

/// Wraps a [`mpsc::Receiver`] as a `Stream` without pulling in a separate
/// `tokio-stream` dependency for the one adaptor this module needs.
fn receiver_stream(
    mut rx: mpsc::Receiver<Bytes>,
) -> impl futures_util::Stream<Item = Result<Bytes, std::convert::Infallible>> {
    futures_util::stream::poll_fn(move |cx| rx.poll_recv(cx).map(|item| item.map(Ok)))
}

/// ADR-045 §3's WebSocket-upgrade passthrough: drive the router through a
/// real `hyper` server connection over an in-process `tokio::io::duplex` pair
/// so axum's `WebSocketUpgrade` extractor sees a genuine
/// `hyper::upgrade::OnUpgrade`, then relay raw bytes both ways as
/// [`ConsoleFrameKind::WsRelay`] frames once the upgrade completes.
///
/// This is the least-precedented piece of this design (see the ADR's own
/// "Risks" section): a directly-built [`Request`] has no `OnUpgrade`
/// extension because nothing drove real HTTP/1.1 framing to produce one, so
/// axum's `WebSocketUpgrade` extractor would otherwise always reject it. This
/// function supplies that framing itself, entirely in-process: one hyper
/// server connection (driving the real router) and one hyper client
/// connection (driving our synthetic request), joined by a duplex pipe.
async fn run_upgrade_stream(
    shared: &Arc<ConnectionShared>,
    open: &ConsoleStreamOpen,
    pinned_host: &str,
    target: Arc<dyn ConsoleDispatchTarget>,
    outbound_credit: &tokio::sync::Semaphore,
    activity: &ActivityClock,
) -> Result<(), ConsoleProxyError> {
    let stream_id = open.stream_id;
    let request = build_request(open, pinned_host, Body::empty())?;

    let (client_half, server_half) = tokio::io::duplex(64 * 1024);

    // Server side: the instance's own console router, driven by hyper's own
    // HTTP/1.1 framing so an upgrade produces a real `OnUpgrade` extension —
    // exactly the path already serving every other WebSocket handler in this
    // codebase, just fed from an in-process duplex instead of a real socket.
    let server_task = tokio::spawn(async move {
        let service = hyper::service::service_fn(move |request: Request<hyper::body::Incoming>| {
            let target = target.clone();
            async move {
                let mut request = request.map(Body::new);
                request.extensions_mut().insert(ConnectInfo(SYNTHETIC_PEER));
                Ok::<Response<Body>, std::convert::Infallible>(target.dispatch(request).await)
            }
        });
        let _ = hyper::server::conn::http1::Builder::new()
            .serve_connection(TokioIo::new(server_half), service)
            .with_upgrades()
            .await;
    });

    let _server_guard = TaskAbortGuard(server_task.abort_handle());
    let (mut sender, connection) =
        hyper::client::conn::http1::handshake::<_, Body>(TokioIo::new(client_half))
            .await
            .map_err(|error| ConsoleProxyError::UpgradeHandshake {
                stream_id,
                reason: error.to_string(),
            })?;
    let connection_task = tokio::spawn(async move {
        let _ = connection.with_upgrades().await;
    });

    let _connection_guard = TaskAbortGuard(connection_task.abort_handle());
    let response = sender.send_request(request).await.map_err(|error| {
        ConsoleProxyError::UpgradeHandshake {
            stream_id,
            reason: error.to_string(),
        }
    })?;

    if response.status() != StatusCode::SWITCHING_PROTOCOLS {
        // The router declined to upgrade (not a WebSocket route, or the
        // handshake itself was rejected) — relay the real response instead of
        // forging an upgrade that never happened.
        let outcome = stream_response(
            shared,
            stream_id,
            ConsoleFrameKind::ResponseBodyChunk,
            outbound_credit,
            activity,
            response.map(Body::new),
            false,
        )
        .await;
        server_task.abort();
        connection_task.abort();
        return outcome;
    }

    validate_response_headers(stream_id, response.headers())?;
    let head = ConsoleResponseHead {
        stream_id,
        status: response.status().as_u16(),
        headers: response
            .headers()
            .iter()
            .filter_map(|(name, value)| {
                value
                    .to_str()
                    .ok()
                    .map(|value| (name.to_string(), value.to_string()))
            })
            .collect(),
        upgraded: true,
    };
    shared
        .send_envelope(ConsoleResponseHead::KIND, &head)
        .await?;

    let upgraded = hyper::upgrade::on(response).await.map_err(|error| {
        ConsoleProxyError::UpgradeHandshake {
            stream_id,
            reason: error.to_string(),
        }
    })?;
    let io = TokioIo::new(upgraded);

    // From here on, inbound `WsRelay` frames are routed to this stream's
    // write half instead of a request body — the one point where a stream's
    // `inbound` target is swapped mid-flight (ADR-045 §3).
    let (relay_tx, mut relay_rx) = mpsc::channel::<Bytes>(STREAM_BODY_CHANNEL_CAPACITY);
    {
        let table = shared.streams.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(entry) = table.get(&stream_id) {
            *entry.inbound.lock().unwrap_or_else(|p| p.into_inner()) = Some(relay_tx);
            entry.remaining_inbound_bytes.store(-1, Ordering::Release);
        }
    }

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let (mut read_half, mut write_half) = tokio::io::split(io);
    let inbound_work = async {
        while let Some(bytes) = relay_rx.recv().await {
            if write_half.write_all(&bytes).await.is_err() {
                break;
            }
        }
    };
    let outbound_work = async {
        let mut read_buf = vec![0u8; 16 * 1024];
        loop {
            match read_half.read(&mut read_buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    send_credited(
                        shared,
                        stream_id,
                        ConsoleFrameKind::WsRelay,
                        outbound_credit,
                        activity,
                        Bytes::copy_from_slice(&read_buf[..n]),
                    )
                    .await?;
                }
            }
        }
        Ok::<(), ConsoleProxyError>(())
    };
    let relay_result = tokio::select! {
        _ = inbound_work => Ok(()),
        result = outbound_work => result,
    };

    server_task.abort();
    connection_task.abort();
    relay_result
}

#[derive(Debug, thiserror::Error)]
enum ConsoleProxyError {
    #[error("could not build the console-proxy channel request: {reason}")]
    InvalidRequest { reason: String },
    #[error("could not connect to the Cloud console-proxy channel: {source}")]
    Connect {
        #[source]
        source: tokio_tungstenite::tungstenite::Error,
    },
    #[error("could not encode a console-proxy envelope: {source}")]
    Encode {
        #[source]
        source: serde_json::Error,
    },
    #[error("could not send a console-proxy frame: {source}")]
    Send {
        #[source]
        source: tokio_tungstenite::tungstenite::Error,
    },
    #[error("timed out connecting to the Cloud console-proxy endpoint")]
    ConnectTimeout,
    #[error("stream {stream_id} response is invalid: {reason}")]
    InvalidStreamResponse { stream_id: Uuid, reason: String },
    #[error("the server hello was unreadable: {reason}")]
    InvalidServerHello { reason: String },
    #[error("timed out waiting for the server hello")]
    HelloTimeout,
    #[error("the connection closed before the server sent its hello")]
    ConnectionClosedDuringHandshake,
    #[error("the console-proxy connection closed")]
    ConnectionClosed,
    #[error("could not build a data frame: {source}")]
    Frame {
        #[source]
        source: ConsoleFrameError,
    },
    #[error("stream {stream_id} request is invalid: {reason}")]
    InvalidStreamRequest { stream_id: Uuid, reason: String },
    #[error("stream {stream_id} response body failed: {reason}")]
    ResponseBody { stream_id: Uuid, reason: String },
    #[error("stream {stream_id} WebSocket upgrade handshake failed: {reason}")]
    UpgradeHandshake { stream_id: Uuid, reason: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::ws::{Message as AxumMessage, WebSocket, WebSocketUpgrade};
    use axum::extract::ConnectInfo as AxumConnectInfo;
    use axum::response::IntoResponse;
    use axum::routing::get;
    use axum::Router;
    use std::io;
    use std::pin::Pin;
    use std::task::{Context as TaskContext, Poll};
    use tokio_tungstenite::tungstenite::protocol::Role;

    fn test_uuid(byte: u8) -> Uuid {
        Uuid::from_u128(u128::from(byte) << 96)
    }

    // -----------------------------------------------------------------
    // Idle timer: an inactivity timeout, never a hard cap on total duration.
    // -----------------------------------------------------------------

    /// Periodic activity — even across a span far longer than
    /// [`CONSOLE_STREAM_IDLE_TIMEOUT`] — must never trip the idle watchdog,
    /// and a real silence of exactly that long must. Run under paused time so
    /// "5 minutes" and "120s idle" cost no real wall-clock time; nothing here
    /// touches a socket, so pausing tokio's clock cannot desync from real I/O.
    #[tokio::test(start_paused = true)]
    async fn idle_clock_survives_periodic_activity_but_fires_after_true_silence() {
        let clock = ActivityClock::new();

        // Activity every 30s for 5 minutes: ten touches, each well inside
        // the 120s idle window that follows it.
        for _ in 0..10 {
            tokio::time::sleep(Duration::from_secs(30)).await;
            clock.touch();
        }
        assert!(
            clock.elapsed() < Duration::from_secs(1),
            "periodic activity must keep resetting the clock, not accumulate toward the \
             120s idle window over the full 5-minute span"
        );

        // Racing the watchdog against a much shorter timer proves it has not
        // already fired despite 5 minutes of wall-clock-equivalent time
        // having passed since the stream opened — the bug this guards
        // against is an absolute cap that would have killed this stream
        // already.
        let watchdog_fired_early = tokio::select! {
            () = idle_watchdog(&clock, CONSOLE_STREAM_IDLE_TIMEOUT) => true,
            () = tokio::time::sleep(Duration::from_millis(1)) => false,
        };
        assert!(
            !watchdog_fired_early,
            "a stream with activity every 30s must still be open after 5 minutes total"
        );

        // Now go fully silent for the idle window: the watchdog must fire,
        // proving this is an inactivity timer and not merely a timer that
        // never fires.
        tokio::time::timeout(
            CONSOLE_STREAM_IDLE_TIMEOUT + Duration::from_secs(5),
            idle_watchdog(&clock, CONSOLE_STREAM_IDLE_TIMEOUT),
        )
        .await
        .expect(
            "the idle watchdog must fire once the stream has been silent for \
             CONSOLE_STREAM_IDLE_TIMEOUT",
        );
    }

    /// A touch that lands while the watchdog is asleep must push the
    /// deadline back rather than being ignored: the watchdog re-reads
    /// `elapsed()` from scratch on every wake instead of trusting a snapshot
    /// taken before it first slept.
    #[tokio::test(start_paused = true)]
    async fn idle_watchdog_re_checks_after_a_late_touch() {
        let clock = Arc::new(ActivityClock::new());
        let watchdog_clock = clock.clone();
        let mut watchdog = tokio::spawn(async move {
            idle_watchdog(&watchdog_clock, CONSOLE_STREAM_IDLE_TIMEOUT).await;
        });

        // Let the watchdog observe ~0 elapsed and go to sleep for ~120s,
        // then touch partway through that sleep.
        tokio::time::sleep(Duration::from_secs(1)).await;
        clock.touch();

        // If the watchdog had trusted its original sleep deadline, it would
        // fire ~119s from here; because it re-reads `elapsed()`, it must
        // instead still be waiting a full 120s after the touch above.
        // `&mut watchdog` works as a select! branch because `JoinHandle` is
        // `Unpin`, so it can be polled repeatedly through a `&mut` without
        // being consumed.
        let fired_before_the_touch_s_own_window = tokio::select! {
            _ = &mut watchdog => true,
            () = tokio::time::sleep(CONSOLE_STREAM_IDLE_TIMEOUT - Duration::from_secs(1)) => false,
        };
        assert!(
            !fired_before_the_touch_s_own_window,
            "a touch during the watchdog's sleep must push the deadline back, not be ignored"
        );

        tokio::time::timeout(Duration::from_secs(5), watchdog)
            .await
            .expect("the watchdog task must not have been abandoned")
            .expect("the watchdog must still fire once the touch's own 120s window elapses");
    }

    // -----------------------------------------------------------------
    // A fake Cloud: hands the raw, already-upgraded socket to the test so
    // each scenario drives the exact protocol exchange it needs, the same
    // way `heartbeat.rs`'s test stub drives the management channel.
    // -----------------------------------------------------------------

    /// Binds a real loopback listener, or returns `None` when the sandbox
    /// denies it — the same graceful skip `heartbeat.rs`/`flusher`'s tests
    /// use.
    async fn fake_cloud_server() -> Option<(String, mpsc::Receiver<WebSocket>)> {
        let (tx, rx) = mpsc::channel(4);
        let app = Router::new()
            .route(
                "/v1/enroll",
                axum::routing::post(|| async {
                    axum::Json(serde_json::json!({
                        "tenant_id": Uuid::new_v4(),
                        "instance_token": "inst_console_proxy_test"
                    }))
                }),
            )
            .route(
                "/v1/console-proxy",
                get(move |ws: WebSocketUpgrade| {
                    let tx = tx.clone();
                    async move {
                        ws.on_upgrade(move |socket| async move {
                            let _ = tx.send(socket).await;
                        })
                    }
                }),
            );
        let listener = match tokio::net::TcpListener::bind::<SocketAddr>(
            "127.0.0.1:0".parse().expect("loopback address must parse"),
        )
        .await
        {
            Ok(listener) => listener,
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
                eprintln!("skipping console-proxy network test: sandbox denied TCP bind");
                return None;
            }
            Err(error) => panic!("test server must bind: {error}"),
        };
        let address = listener.local_addr().expect("test server has an address");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Some((format!("http://{address}"), rx))
    }

    async fn linked_test_link(backend_url: &str) -> (Arc<CloudLink>, tempfile::TempDir) {
        let directory = tempfile::tempdir().expect("temporary directory must be created");
        let link = Arc::new(CloudLink::load_for_loopback_development(
            directory.path().to_path_buf(),
            "console-proxy-test",
        ));
        link.configure(
            BackendUrl::loopback_development(backend_url)
                .expect("stub backend URL must be accepted"),
        )
        .expect("test link must be configured");
        link.enroll("console-proxy-test-code")
            .await
            .expect("test link must enroll");
        (link, directory)
    }

    enum WireFrame {
        Control(Envelope),
        Data(ConsoleDataFrame),
    }

    async fn recv_wire_frame(socket: &mut WebSocket) -> Option<WireFrame> {
        loop {
            match socket.recv().await {
                None => return None,
                Some(Ok(AxumMessage::Close(_))) => return None,
                Some(Ok(AxumMessage::Text(text))) => {
                    if let Ok(envelope) = serde_json::from_str::<Envelope>(&text) {
                        return Some(WireFrame::Control(envelope));
                    }
                }
                Some(Ok(AxumMessage::Binary(bytes))) => {
                    if let Ok(frame) = ConsoleDataFrame::decode(&bytes) {
                        return Some(WireFrame::Data(frame));
                    }
                }
                Some(Ok(_)) => continue,
                Some(Err(_)) => return None,
            }
        }
    }

    async fn recv_control(socket: &mut WebSocket) -> Envelope {
        match recv_wire_frame(socket).await {
            Some(WireFrame::Control(envelope)) => envelope,
            Some(WireFrame::Data(frame)) => {
                panic!(
                    "expected a control frame, got a data frame ({:?})",
                    frame.frame_kind
                )
            }
            None => panic!("connection closed while a control frame was expected"),
        }
    }

    async fn send_control(socket: &mut WebSocket, kind: &str, payload: &impl serde::Serialize) {
        let envelope = Envelope::new(kind, payload).expect("envelope must encode");
        let text = serde_json::to_string(&envelope).expect("envelope must serialize");
        socket
            .send(AxumMessage::Text(text.into()))
            .await
            .expect("control frame must send");
    }

    async fn send_data(
        socket: &mut WebSocket,
        kind: ConsoleFrameKind,
        stream_id: Uuid,
        payload: Bytes,
    ) {
        let frame = ConsoleDataFrame::new(kind, stream_id, payload).expect("data frame must build");
        socket
            .send(AxumMessage::Binary(frame.encode()))
            .await
            .expect("data frame must send");
    }

    /// Send the server's opening `Hello` and read back the client's reply,
    /// asserting it negotiated `ConsoleProxy` — the same handshake shape
    /// [`super::handshake`] performs from the other side.
    async fn cloud_handshake(socket: &mut WebSocket) {
        let hello = Hello {
            protocol_version: PROTOCOL_VERSION,
            agent_version: "cloud-test-backend".into(),
            capabilities: vec![Capability::ConsoleProxy],
        };
        send_control(socket, "hello", &hello).await;
        let envelope = recv_control(socket).await;
        assert_eq!(envelope.kind, "hello");
        let client_hello: Hello = envelope.decode("hello").expect("client hello must decode");
        assert!(client_hello
            .capabilities
            .contains(&Capability::ConsoleProxy));
    }

    const PINNED_HOST: &str = "console.example.invalid";

    async fn send_oidc_config(socket: &mut WebSocket) {
        send_control(
            socket,
            ConsoleOidcConfig::KIND,
            &ConsoleOidcConfig {
                issuer: "https://issuer.example.invalid".into(),
                client_id: "instance-client".into(),
                client_secret: "test-secret".into(),
                jwks_uri: "https://issuer.example.invalid/jwks.json".into(),
                console_host: PINNED_HOST.into(),
            },
        )
        .await;
    }

    fn open_request(stream_id: Uuid, method: &str, path: &str) -> ConsoleStreamOpen {
        ConsoleStreamOpen {
            stream_id,
            method: method.into(),
            path: path.into(),
            query: None,
            headers: vec![("host".into(), PINNED_HOST.into())],
            client_ip: Some("203.0.113.5".into()),
            upgrade_requested: false,
        }
    }

    struct TestOidcSink;
    #[async_trait::async_trait]
    impl ConsoleOidcSink for TestOidcSink {
        async fn on_config(&self, _: ConsoleOidcConfig) -> Result<(), String> {
            Ok(())
        }
        async fn on_revoke(&self) {}
    }

    #[tokio::test]
    async fn enabling_console_wakes_worker_without_waiting_for_retry() {
        let Some((backend_url, mut server_rx)) = fake_cloud_server().await else {
            return;
        };
        let (link, _dir) = linked_test_link(&backend_url).await;
        let (enabled_tx, enabled_rx) = watch::channel(false);
        let (join, cancel_tx) = ConsoleProxyWorker::spawn(
            link,
            enabled_rx,
            ConsoleDispatchSlot::new(),
            Arc::new(TestOidcSink),
        );
        // Let the disabled worker enter its five-second retry sleep.
        assert!(
            tokio::time::timeout(Duration::from_millis(50), server_rx.recv())
                .await
                .is_err()
        );
        enabled_tx.send(true).unwrap();
        let mut cloud = tokio::time::timeout(Duration::from_secs(2), server_rx.recv())
            .await
            .expect("enable must wake the worker promptly")
            .expect("worker must connect");
        cloud_handshake(&mut cloud).await;
        cancel_tx.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(5), join)
            .await
            .expect("worker must stop")
            .unwrap();
    }

    #[test]
    fn short_console_connections_preserve_outage_backoff() {
        let mut retry = Duration::ZERO;
        for _ in 0..20 {
            let outcome = connection_outcome(CycleOutcome::Disconnected, Duration::from_secs(1));
            assert_eq!(outcome, CycleOutcome::Disconnected);
            retry = next_reconnect_interval(retry, outcome);
        }
        assert_eq!(retry, MAX_RECONNECT_INTERVAL);
        let recovered = connection_outcome(CycleOutcome::Disconnected, STABLE_CONNECTION_INTERVAL);
        assert_eq!(
            next_reconnect_interval(retry, recovered),
            BASE_RECONNECT_INTERVAL
        );
        assert_eq!(
            connection_outcome(CycleOutcome::Cancelled, STABLE_CONNECTION_INTERVAL),
            CycleOutcome::Cancelled,
        );
    }

    #[test]
    fn successful_console_connection_resets_outage_backoff() {
        assert_eq!(
            next_reconnect_interval(MAX_RECONNECT_INTERVAL, CycleOutcome::Connected),
            BASE_RECONNECT_INTERVAL,
        );
        assert_eq!(
            next_reconnect_interval(BASE_RECONNECT_INTERVAL, CycleOutcome::Disconnected),
            BASE_RECONNECT_INTERVAL * 2,
        );
    }

    struct Harness {
        _dir: tempfile::TempDir,
        dispatch: ConsoleDispatchSlot,
        enabled_tx: watch::Sender<bool>,
        cancel_tx: watch::Sender<bool>,
        join: JoinHandle<()>,
    }

    impl Harness {
        async fn start(backend_url: &str) -> Self {
            Self::start_with_sink(backend_url, Arc::new(TestOidcSink)).await
        }
        async fn start_with_sink(backend_url: &str, sink: Arc<dyn ConsoleOidcSink>) -> Self {
            let (link, dir) = linked_test_link(backend_url).await;
            let dispatch = ConsoleDispatchSlot::new();
            let (enabled_tx, enabled_rx) = watch::channel(true);
            let (join, cancel_tx) =
                ConsoleProxyWorker::spawn(link, enabled_rx, dispatch.clone(), sink);
            Self {
                _dir: dir,
                dispatch,
                enabled_tx,
                cancel_tx,
                join,
            }
        }

        async fn set_router(&self, router: axum::Router) {
            self.dispatch
                .set(Arc::new(ConsoleRouterHandle::new(router)))
                .await;
        }
    }

    async fn expect_response(
        socket: &mut WebSocket,
        stream_id: Uuid,
    ) -> (ConsoleResponseHead, Vec<u8>) {
        let envelope = recv_control(socket).await;
        assert_eq!(envelope.kind, ConsoleResponseHead::KIND);
        let head: ConsoleResponseHead = envelope
            .decode(ConsoleResponseHead::KIND)
            .expect("response head must decode");
        assert_eq!(head.stream_id, stream_id);

        let mut body = Vec::new();
        loop {
            match recv_wire_frame(socket).await.expect("stream must end") {
                WireFrame::Data(frame) => {
                    assert_eq!(frame.frame_kind, ConsoleFrameKind::ResponseBodyChunk);
                    assert_eq!(frame.stream_id, stream_id);
                    body.extend_from_slice(frame.payload());
                }
                WireFrame::Control(envelope) if envelope.kind == ConsoleStreamEnd::KIND => {
                    let end: ConsoleStreamEnd = envelope
                        .decode(ConsoleStreamEnd::KIND)
                        .expect("stream end must decode");
                    assert_eq!(end.stream_id, stream_id);
                    assert_eq!(end.reason, ConsoleStreamEndReason::Complete);
                    break;
                }
                WireFrame::Control(envelope) => {
                    panic!(
                        "unexpected control frame while draining a response: {}",
                        envelope.kind
                    )
                }
            }
        }
        (head, body)
    }

    // -----------------------------------------------------------------
    // Scenarios
    // -----------------------------------------------------------------

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn fast_streams_release_slots_after_every_response() {
        let Some((url, mut server_rx)) = fake_cloud_server().await else {
            return;
        };
        let harness = Harness::start(&url).await;
        harness
            .set_router(Router::new().route("/fast", get(|| async { "ok" })))
            .await;
        let mut cloud = server_rx.recv().await.unwrap();
        cloud_handshake(&mut cloud).await;
        send_oidc_config(&mut cloud).await;
        for _ in 0..100 {
            let id = Uuid::new_v4();
            send_control(
                &mut cloud,
                ConsoleStreamOpen::KIND,
                &open_request(id, "GET", "/fast"),
            )
            .await;
            let (_, body) =
                tokio::time::timeout(Duration::from_secs(3), expect_response(&mut cloud, id))
                    .await
                    .unwrap();
            assert_eq!(body, b"ok");
        }
        harness.cancel_tx.send(true).unwrap();
        harness.join.await.unwrap();
    }

    #[tokio::test]
    async fn failed_oidc_provisioning_leaves_console_routing_disabled() {
        let Some((backend_url, mut server_rx)) = fake_cloud_server().await else {
            return;
        };
        let harness = Harness::start_with_sink(&backend_url, Arc::new(NoopConsoleOidcSink)).await;
        harness
            .set_router(axum::Router::new().route("/", get(|| async { "unexpected dispatch" })))
            .await;
        let mut cloud = server_rx.recv().await.unwrap();
        cloud_handshake(&mut cloud).await;
        send_oidc_config(&mut cloud).await;
        let open = open_request(test_uuid(90), "GET", "/");
        send_control(&mut cloud, ConsoleStreamOpen::KIND, &open).await;
        assert_eq!(
            recv_control(&mut cloud).await.kind,
            ConsoleStreamRefused::KIND
        );
        harness.cancel_tx.send(true).unwrap();
        harness.join.await.unwrap();
    }

    #[tokio::test]
    async fn missing_or_malformed_client_ip_is_refused() {
        let Some((url, mut server_rx)) = fake_cloud_server().await else {
            return;
        };
        let harness = Harness::start(&url).await;
        harness
            .set_router(Router::new().route("/fast", get(|| async { "unexpected dispatch" })))
            .await;
        let mut cloud = server_rx.recv().await.unwrap();
        cloud_handshake(&mut cloud).await;
        send_oidc_config(&mut cloud).await;
        for ip in [
            None,
            Some("not-an-ip".into()),
            Some("203.0.113.7, 127.0.0.1".into()),
        ] {
            let mut request = open_request(Uuid::new_v4(), "GET", "/fast");
            request.client_ip = ip;
            send_control(&mut cloud, ConsoleStreamOpen::KIND, &request).await;
            assert_eq!(
                recv_control(&mut cloud).await.kind,
                ConsoleStreamRefused::KIND
            );
        }
        harness.cancel_tx.send(true).unwrap();
        harness.join.await.unwrap();
    }

    #[tokio::test]
    async fn remote_revoke_closes_active_streams_and_refuses_new_requests() {
        let Some((url, mut server_rx)) = fake_cloud_server().await else {
            return;
        };
        let harness = Harness::start(&url).await;
        harness
            .set_router(Router::new().route(
                "/hang",
                get(|| async {
                    std::future::pending::<()>().await;
                    "never"
                }),
            ))
            .await;
        let mut cloud = server_rx.recv().await.unwrap();
        cloud_handshake(&mut cloud).await;
        send_oidc_config(&mut cloud).await;
        let id = Uuid::new_v4();
        send_control(
            &mut cloud,
            ConsoleStreamOpen::KIND,
            &open_request(id, "GET", "/hang"),
        )
        .await;
        send_control(&mut cloud, ConsoleOidcRevoke::KIND, &ConsoleOidcRevoke).await;
        let end: ConsoleStreamEnd = recv_control(&mut cloud)
            .await
            .decode(ConsoleStreamEnd::KIND)
            .unwrap();
        assert_eq!(end.stream_id, id);
        assert_eq!(end.reason, ConsoleStreamEndReason::GoingAway);
        send_control(
            &mut cloud,
            ConsoleStreamOpen::KIND,
            &open_request(Uuid::new_v4(), "GET", "/hang"),
        )
        .await;
        assert_eq!(
            recv_control(&mut cloud).await.kind,
            ConsoleStreamRefused::KIND
        );
        harness.cancel_tx.send(true).unwrap();
        harness.join.await.unwrap();
    }

    #[tokio::test]
    async fn a_plain_get_is_dispatched_and_streamed_back() {
        let Some((backend_url, mut server_rx)) = fake_cloud_server().await else {
            return;
        };
        let harness = Harness::start(&backend_url).await;
        harness
            .set_router(axum::Router::new().route("/hello", get(|| async { "hi there" })))
            .await;

        let mut cloud = server_rx.recv().await.expect("worker must connect");
        cloud_handshake(&mut cloud).await;
        send_oidc_config(&mut cloud).await;

        let stream_id = test_uuid(1);
        send_control(
            &mut cloud,
            ConsoleStreamOpen::KIND,
            &open_request(stream_id, "GET", "/hello"),
        )
        .await;

        let (head, body) = expect_response(&mut cloud, stream_id).await;
        assert_eq!(head.status, 200);
        assert!(!head.upgraded);
        assert_eq!(String::from_utf8(body).expect("utf8 body"), "hi there");

        harness.cancel_tx.send(true).expect("cancel must send");
        let _ = tokio::time::timeout(Duration::from_secs(5), harness.join).await;
    }

    #[tokio::test]
    async fn resolve_client_ip_sees_the_forwarded_browser_ip() {
        let Some((backend_url, mut server_rx)) = fake_cloud_server().await else {
            return;
        };
        let harness = Harness::start(&backend_url).await;
        harness
            .set_router(axum::Router::new().route(
                "/whoami",
                get(
                    |AxumConnectInfo(peer): AxumConnectInfo<SocketAddr>,
                     headers: axum::http::HeaderMap| async move {
                        temps_core::resolve_client_ip(&headers, Some(peer))
                    },
                ),
            ))
            .await;

        let mut cloud = server_rx.recv().await.expect("worker must connect");
        cloud_handshake(&mut cloud).await;
        send_oidc_config(&mut cloud).await;

        let stream_id = test_uuid(2);
        send_control(
            &mut cloud,
            ConsoleStreamOpen::KIND,
            &open_request(stream_id, "GET", "/whoami"),
        )
        .await;

        let (head, body) = expect_response(&mut cloud, stream_id).await;
        assert_eq!(head.status, 200);
        assert_eq!(
            String::from_utf8(body).expect("utf8 body"),
            "203.0.113.5",
            "the resolved IP must be the browser's real address forwarded by Cloud, \
             proving the synthetic ConnectInfo(loopback) + X-Forwarded-For synthesis works"
        );

        harness.cancel_tx.send(true).expect("cancel must send");
        let _ = tokio::time::timeout(Duration::from_secs(5), harness.join).await;
    }

    #[test]
    fn browser_proxy_headers_cannot_override_trusted_metadata() {
        let mut open = open_request(Uuid::new_v4(), "GET", "/whoami");
        open.headers.extend([
            ("x-forwarded-for".into(), "198.51.100.99".into()),
            ("x-forwarded-for".into(), "127.0.0.1".into()),
            ("x-real-ip".into(), "127.0.0.1".into()),
            ("x-forwarded-proto".into(), "http".into()),
            ("x-forwarded-host".into(), "spoof.example.invalid".into()),
            ("forwarded".into(), "for=127.0.0.1;proto=http".into()),
        ]);
        let request = build_request(&open, PINNED_HOST, Body::empty()).unwrap();
        assert_eq!(
            temps_core::resolve_client_ip(request.headers(), Some(SYNTHETIC_PEER)),
            "203.0.113.5"
        );
        assert_eq!(
            request.headers().get_all("x-forwarded-for").iter().count(),
            1
        );
        assert_eq!(request.headers()["x-forwarded-proto"], "https");
        assert!(!request.headers().contains_key("forwarded"));
        assert!(!request.headers().contains_key("x-forwarded-host"));
        assert!(!request.headers().contains_key("x-real-ip"));
    }

    #[tokio::test]
    async fn oversized_request_headers_end_without_dispatch_and_allow_next_request() {
        let Some((url, mut server_rx)) = fake_cloud_server().await else {
            return;
        };
        let harness = Harness::start(&url).await;
        harness
            .set_router(Router::new().route("/fast", get(|| async { "ok" })))
            .await;
        let mut cloud = server_rx.recv().await.unwrap();
        cloud_handshake(&mut cloud).await;
        send_oidc_config(&mut cloud).await;
        let mut open = open_request(Uuid::new_v4(), "GET", "/fast");
        open.headers
            .push(("x-oversized".into(), "x".repeat(CONSOLE_MAX_HEADER_BYTES)));
        send_control(&mut cloud, ConsoleStreamOpen::KIND, &open).await;
        let envelope = tokio::time::timeout(Duration::from_secs(1), recv_control(&mut cloud))
            .await
            .expect("invalid request must get a terminal response");
        let end: ConsoleStreamEnd = envelope.decode(ConsoleStreamEnd::KIND).unwrap();
        assert_eq!(end.stream_id, open.stream_id);
        assert!(matches!(end.reason, ConsoleStreamEndReason::Error { .. }));
        let next = open_request(Uuid::new_v4(), "GET", "/fast");
        send_control(&mut cloud, ConsoleStreamOpen::KIND, &next).await;
        assert_eq!(expect_response(&mut cloud, next.stream_id).await.1, b"ok");
        harness.cancel_tx.send(true).unwrap();
        harness.join.await.unwrap();
    }

    #[test]
    fn connection_scoped_request_headers_do_not_reach_the_router() {
        let mut open = open_request(Uuid::new_v4(), "GET", "/");
        open.headers.extend([
            ("connection".into(), "keep-alive, X-Private-Hop".into()),
            ("x-private-hop".into(), "private".into()),
            ("keep-alive".into(), "timeout=30".into()),
            ("proxy-authorization".into(), "private-credential".into()),
            ("cookie".into(), "session=browser-session".into()),
        ]);
        let request = build_request(&open, PINNED_HOST, Body::empty()).unwrap();
        for name in [
            "connection",
            "x-private-hop",
            "keep-alive",
            "proxy-authorization",
        ] {
            assert!(
                !request.headers().contains_key(name),
                "{name} is connection-scoped"
            );
        }
        assert_eq!(request.headers()["cookie"], "session=browser-session");
    }

    #[tokio::test]
    async fn console_html_redirect_and_session_cookies_round_trip_over_the_wire() {
        let Some((url, mut server_rx)) = fake_cloud_server().await else {
            return;
        };
        let harness = Harness::start(&url).await;
        harness.set_router(Router::new()
            .route("/", get(|| async { axum::response::Html("<html><body>Console</body></html>") }))
            .route("/signin", get(|| async {
                let mut response = Response::builder().status(StatusCode::SEE_OTHER)
                    .header("location", "/")
                    .header("connection", "keep-alive, x-private-hop")
                    .header("x-private-hop", "private")
                    .body(Body::empty()).unwrap();
                response.headers_mut().append("set-cookie", HeaderValue::from_static("session=browser-session; Path=/; Secure; HttpOnly; SameSite=Strict"));
                response.headers_mut().append("set-cookie", HeaderValue::from_static("oidc_state=; Path=/; Max-Age=0; Secure; HttpOnly"));
                response
            }))
            .route("/api/user/me", get(|headers: axum::http::HeaderMap| async move {
                if headers.get("cookie").and_then(|value| value.to_str().ok()) == Some("session=browser-session") {
                    (StatusCode::OK, "authenticated")
                } else {
                    (StatusCode::UNAUTHORIZED, "sign in")
                }
            }))).await;
        let mut cloud = server_rx.recv().await.unwrap();
        cloud_handshake(&mut cloud).await;
        send_oidc_config(&mut cloud).await;
        let page = open_request(Uuid::new_v4(), "GET", "/");
        send_control(&mut cloud, ConsoleStreamOpen::KIND, &page).await;
        let (head, body) = expect_response(&mut cloud, page.stream_id).await;
        assert_eq!(head.status, 200);
        assert!(String::from_utf8(body).unwrap().contains("Console"));
        let login = open_request(Uuid::new_v4(), "GET", "/signin");
        send_control(&mut cloud, ConsoleStreamOpen::KIND, &login).await;
        let (head, _) = expect_response(&mut cloud, login.stream_id).await;
        assert_eq!(head.status, 303);
        assert_eq!(header_value(&head.headers, "location"), Some("/"));
        let cookies: Vec<_> = head
            .headers
            .iter()
            .filter(|(name, _)| name == "set-cookie")
            .collect();
        assert_eq!(
            cookies.len(),
            2,
            "session creation and state cleanup must remain separate cookies"
        );
        assert!(cookies
            .iter()
            .any(|(_, value)| value.contains("Secure; HttpOnly; SameSite=Strict")));
        assert!(cookies.iter().any(|(_, value)| value.contains("Max-Age=0")));
        assert!(header_value(&head.headers, "x-private-hop").is_none());
        assert!(header_value(&head.headers, "connection").is_none());
        for authenticated in [false, true] {
            let mut open = open_request(Uuid::new_v4(), "GET", "/api/user/me");
            if authenticated {
                open.headers
                    .push(("cookie".into(), "session=browser-session".into()));
            }
            send_control(&mut cloud, ConsoleStreamOpen::KIND, &open).await;
            let (head, _) = expect_response(&mut cloud, open.stream_id).await;
            assert_eq!(head.status, if authenticated { 200 } else { 401 });
        }
        harness.cancel_tx.send(true).unwrap();
        harness.join.await.unwrap();
    }

    /// Browser dependencies are optional in Rust-only CI jobs. Where available,
    /// drive Chromium through a local Cloud HTTP stub and the real relay worker.
    /// The sign-in route tests cookie transport, not the external Cloud IdP.
    #[tokio::test]
    async fn chromium_loads_relayed_html_keeps_cookies_and_receives_sse() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let playwright = root.join("web/node_modules/@playwright/test/index.mjs");
        if !playwright.exists() {
            eprintln!(
                "skipping browser relay test: install web dependencies and Playwright Chromium"
            );
            return;
        }
        let module = serde_json::to_string(&playwright.to_string_lossy()).unwrap();
        let probe = format!(
            "const {{chromium}} = await import({module}); console.log(chromium.executablePath());"
        );
        let available = tokio::process::Command::new("bun")
            .args(["-e", &probe])
            .output()
            .await;
        let Ok(available) = available else {
            eprintln!("skipping browser relay test: Bun is unavailable");
            return;
        };
        let executable = String::from_utf8_lossy(&available.stdout);
        if !available.status.success() || !std::path::Path::new(executable.trim()).exists() {
            eprintln!("skipping browser relay test: Playwright Chromium is unavailable");
            return;
        }
        let Some((url, mut server_rx)) = fake_cloud_server().await else {
            return;
        };
        let harness = Harness::start(&url).await;
        harness
            .set_router(
                Router::new()
                    .route(
                        "/",
                        get(|| async {
                            axum::response::Html("<html><body><h1>Relay console</h1></body></html>")
                        }),
                    )
                    .route(
                        "/signin",
                        get(|| async {
                            Response::builder()
                                .status(303)
                                .header("location", "/")
                                .header(
                                    "set-cookie",
                                    "session=browser-session; Path=/; HttpOnly; SameSite=Strict",
                                )
                                .body(Body::empty())
                                .unwrap()
                        }),
                    )
                    .route(
                        "/api/user/me",
                        get(|headers: axum::http::HeaderMap| async move {
                            if headers.get("cookie").and_then(|value| value.to_str().ok())
                                == Some("session=browser-session")
                            {
                                (StatusCode::OK, "authenticated")
                            } else {
                                (StatusCode::UNAUTHORIZED, "sign in")
                            }
                        }),
                    )
                    .route(
                        "/events",
                        get(|| async {
                            use axum::response::sse::{Event, Sse};
                            Sse::new(
                                futures_util::stream::once(async {
                                    Ok::<_, std::convert::Infallible>(
                                        Event::default().data("live event"),
                                    )
                                })
                                .chain(futures_util::stream::pending()),
                            )
                        }),
                    ),
            )
            .await;
        let mut cloud = server_rx.recv().await.unwrap();
        cloud_handshake(&mut cloud).await;
        send_oidc_config(&mut cloud).await;

        type BrowserReply = tokio::sync::oneshot::Sender<Response<Body>>;
        let (requests_tx, mut requests_rx) = mpsc::channel::<(ConsoleStreamOpen, BrowserReply)>(16);
        let front = Router::new().fallback(move |request: Request<Body>| {
            let tx = requests_tx.clone();
            async move {
                let mut open = open_request(
                    Uuid::new_v4(),
                    request.method().as_str(),
                    request.uri().path(),
                );
                open.query = request.uri().query().map(str::to_owned);
                open.headers.extend(
                    request
                        .headers()
                        .iter()
                        .filter(|(name, _)| name.as_str() != "host")
                        .map(|(name, value)| {
                            (name.to_string(), value.to_str().unwrap().to_owned())
                        }),
                );
                let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
                tx.send((open, reply_tx)).await.unwrap();
                reply_rx.await.unwrap()
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let browser_url = format!("http://{}", listener.local_addr().unwrap());
        let front_task = tokio::spawn(async move {
            axum::serve(listener, front).await.unwrap();
        });
        let _front_guard = TaskAbortGuard(front_task.abort_handle());
        let pump = tokio::spawn(async move {
            type PendingReply = (
                Option<(BrowserReply, mpsc::Receiver<Bytes>)>,
                mpsc::Sender<Bytes>,
            );
            let mut pending: HashMap<Uuid, PendingReply> = HashMap::new();
            loop {
                tokio::select! {
                    request = requests_rx.recv() => {
                        let Some((open, reply)) = request else { break; };
                        let (body_tx, body_rx) = mpsc::channel(8);
                        pending.insert(open.stream_id, (Some((reply, body_rx)), body_tx));
                        send_control(&mut cloud, ConsoleStreamOpen::KIND, &open).await;
                    }
                    frame = recv_wire_frame(&mut cloud) => {
                        match frame {
                            Some(WireFrame::Control(envelope)) if envelope.kind == ConsoleResponseHead::KIND => {
                                let head: ConsoleResponseHead = envelope.decode(ConsoleResponseHead::KIND).unwrap();
                                let entry = pending.get_mut(&head.stream_id).unwrap();
                                let (reply, rx) = entry.0.take().unwrap();
                                let mut response = Response::builder().status(head.status);
                                for (name, value) in head.headers { response = response.header(name, value); }
                                let _ = reply.send(response.body(Body::from_stream(receiver_stream(rx))).unwrap());
                            }
                            Some(WireFrame::Data(frame)) => {
                                if let Some(entry) = pending.get(&frame.stream_id) {
                                    let _ = entry.1.send(frame.payload().clone()).await;
                                    send_control(&mut cloud, ConsoleWindowUpdate::KIND, &ConsoleWindowUpdate {
                                        stream_id: frame.stream_id, additional_bytes: frame.payload().len() as u32,
                                    }).await;
                                }
                            }
                            Some(WireFrame::Control(envelope)) if envelope.kind == ConsoleStreamEnd::KIND => {
                                let end: ConsoleStreamEnd = envelope.decode(ConsoleStreamEnd::KIND).unwrap();
                                pending.remove(&end.stream_id);
                            }
                            None => break,
                            Some(_) => {}
                        }
                    }
                }
            }
        });
        let _pump_guard = TaskAbortGuard(pump.abort_handle());
        let url = serde_json::to_string(&browser_url).unwrap();
        let script = format!(
            r#"
            const {{ chromium }} = await import({module});
            const browser = await chromium.launch({{headless: true}});
            try {{
                const page = await browser.newPage();
                await page.goto({url});
                if (await page.locator('h1').textContent() !== 'Relay console') throw Error('HTML did not load');
                const result = await page.evaluate(async () => {{
                    const before = (await fetch('/api/user/me')).status;
                    await fetch('/signin');
                    const after = await (await fetch('/api/user/me')).text();
                    const event = await new Promise((resolve, reject) => {{
                        const events = new EventSource('/events');
                        events.onmessage = value => {{ events.close(); resolve(value.data); }};
                        events.onerror = () => {{ events.close(); reject(Error('SSE failed')); }};
                    }});
                    return {{before, after, event, cookie: document.cookie}};
                }});
                if (result.before !== 401 || result.after !== 'authenticated' || result.event !== 'live event' || result.cookie.includes('session=')) {{
                    throw Error(JSON.stringify(result));
                }}
                console.log('browser relay passed');
            }} finally {{ await browser.close(); }}
        "#
        );
        let result = tokio::time::timeout(
            Duration::from_secs(30),
            tokio::process::Command::new("bun")
                .args(["-e", &script])
                .kill_on_drop(true)
                .output(),
        )
        .await
        .expect("browser flow must finish promptly")
        .unwrap();
        assert!(
            result.status.success(),
            "browser flow failed: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(String::from_utf8_lossy(&result.stdout).contains("browser relay passed"));
        harness.cancel_tx.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(6), harness.join)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn oversized_response_headers_end_the_stream_without_sending_an_oversized_head() {
        let Some((url, mut server_rx)) = fake_cloud_server().await else {
            return;
        };
        let harness = Harness::start(&url).await;
        harness
            .set_router(Router::new().route(
                "/",
                get(|| async {
                    Response::builder()
                        .header("x-oversized", "x".repeat(CONSOLE_MAX_HEADER_BYTES))
                        .body(Body::empty())
                        .unwrap()
                }),
            ))
            .await;
        let mut cloud = server_rx.recv().await.unwrap();
        cloud_handshake(&mut cloud).await;
        send_oidc_config(&mut cloud).await;
        let open = open_request(Uuid::new_v4(), "GET", "/");
        send_control(&mut cloud, ConsoleStreamOpen::KIND, &open).await;
        let end: ConsoleStreamEnd = recv_control(&mut cloud)
            .await
            .decode(ConsoleStreamEnd::KIND)
            .unwrap();
        assert_eq!(end.stream_id, open.stream_id);
        assert!(matches!(end.reason, ConsoleStreamEndReason::Error { .. }));
        harness.cancel_tx.send(true).unwrap();
        harness.join.await.unwrap();
    }

    #[tokio::test]
    async fn disabling_under_request_body_backpressure_still_closes_the_relay_promptly() {
        let Some((url, mut server_rx)) = fake_cloud_server().await else {
            return;
        };
        let harness = Harness::start(&url).await;
        harness
            .set_router(Router::new().route(
                "/blocked",
                axum::routing::post(|request: Request<Body>| async move {
                    std::future::pending::<()>().await;
                    drop(request);
                    StatusCode::OK
                }),
            ))
            .await;
        let mut cloud = server_rx.recv().await.unwrap();
        cloud_handshake(&mut cloud).await;
        send_oidc_config(&mut cloud).await;
        let mut open = open_request(Uuid::new_v4(), "POST", "/blocked");
        open.headers
            .push(("content-length".into(), "1048576".into()));
        send_control(&mut cloud, ConsoleStreamOpen::KIND, &open).await;
        for _ in 0..=STREAM_BODY_CHANNEL_CAPACITY {
            send_data(
                &mut cloud,
                ConsoleFrameKind::RequestBodyChunk,
                open.stream_id,
                Bytes::from_static(b"pending body"),
            )
            .await;
        }
        for _ in 0..STREAM_BODY_CHANNEL_CAPACITY {
            assert_eq!(
                recv_control(&mut cloud).await.kind,
                ConsoleWindowUpdate::KIND
            );
        }
        harness.enabled_tx.send(false).unwrap();
        let envelope = tokio::time::timeout(Duration::from_secs(2), recv_control(&mut cloud))
            .await
            .expect("disable must interrupt a blocked body delivery");
        let end: ConsoleStreamEnd = envelope.decode(ConsoleStreamEnd::KIND).unwrap();
        assert_eq!(end.stream_id, open.stream_id);
        assert_eq!(end.reason, ConsoleStreamEndReason::GoingAway);
        harness.cancel_tx.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(2), harness.join)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn a_backend_that_never_upgrades_hits_the_connect_deadline() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let backend =
            BackendUrl::loopback_development(&format!("http://{}", listener.local_addr().unwrap()))
                .unwrap();
        let task = tokio::spawn(async move { connect(&backend, "test-token").await });
        let (socket, _) = tokio::time::timeout(Duration::from_secs(2), listener.accept())
            .await
            .unwrap()
            .unwrap();
        tokio::time::pause();
        tokio::time::advance(HANDSHAKE_TIMEOUT + Duration::from_secs(1)).await;
        assert!(matches!(
            task.await.unwrap(),
            Err(ConsoleProxyError::ConnectTimeout)
        ));
        drop(socket);
    }

    #[tokio::test(start_paused = true)]
    async fn teardown_aborts_every_stream_even_when_the_writer_queue_is_full() {
        let (tx, _rx) = mpsc::channel(1);
        tx.send(Message::Text("blocked".into())).await.unwrap();
        let shared = ConnectionShared {
            console_host: StdRwLock::new(Some(PINNED_HOST.into())),
            streams: StdMutex::new(HashMap::new()),
            outbound: tx,
            dispatch: ConsoleDispatchSlot::new(),
        };
        let mut tasks = Vec::new();
        for _ in 0..CONSOLE_MAX_CONCURRENT_STREAMS {
            let task = tokio::spawn(std::future::pending::<()>());
            shared.streams.lock().unwrap().insert(
                Uuid::new_v4(),
                StreamEntry {
                    inbound: Arc::new(StdMutex::new(None)),
                    remaining_inbound_bytes: Arc::new(AtomicI64::new(-1)),
                    outbound_credit: Arc::new(tokio::sync::Semaphore::new(0)),
                    activity: Arc::new(ActivityClock::new()),
                    abort: task.abort_handle(),
                },
            );
            tasks.push(task);
        }
        tokio::time::timeout(
            STREAM_SHUTDOWN_TIMEOUT + Duration::from_secs(1),
            close_all_streams(&shared, ConsoleStreamEndReason::GoingAway),
        )
        .await
        .expect("shutdown must be bounded even if Cloud stops consuming data");
        assert!(shared.streams.lock().unwrap().is_empty());
        for task in tasks {
            assert!(task.await.unwrap_err().is_cancelled());
        }
    }

    #[tokio::test]
    async fn cancelling_a_pending_upgrade_releases_its_router_dispatch() {
        struct DispatchDrop(Arc<tokio::sync::Notify>);
        impl Drop for DispatchDrop {
            fn drop(&mut self) {
                self.0.notify_one();
            }
        }
        let Some((url, mut server_rx)) = fake_cloud_server().await else {
            return;
        };
        let harness = Harness::start(&url).await;
        let started = Arc::new(tokio::sync::Notify::new());
        let dropped = Arc::new(tokio::sync::Notify::new());
        let handler_started = started.clone();
        let handler_dropped = dropped.clone();
        harness
            .set_router(Router::new().route(
                "/ws",
                get(move |request: Request<Body>| {
                    let started = handler_started.clone();
                    let dropped = handler_dropped.clone();
                    async move {
                        let _drop = DispatchDrop(dropped);
                        started.notify_one();
                        std::future::pending::<()>().await;
                        drop(request);
                        StatusCode::OK
                    }
                }),
            ))
            .await;
        let mut cloud = server_rx.recv().await.unwrap();
        cloud_handshake(&mut cloud).await;
        send_oidc_config(&mut cloud).await;
        let mut open = open_request(Uuid::new_v4(), "GET", "/ws");
        open.upgrade_requested = true;
        open.headers.extend([
            ("origin".into(), format!("https://{PINNED_HOST}")),
            ("connection".into(), "Upgrade".into()),
            ("upgrade".into(), "websocket".into()),
        ]);
        send_control(&mut cloud, ConsoleStreamOpen::KIND, &open).await;
        tokio::time::timeout(Duration::from_secs(2), started.notified())
            .await
            .unwrap();
        send_control(
            &mut cloud,
            ConsoleStreamCancel::KIND,
            &ConsoleStreamCancel {
                stream_id: open.stream_id,
            },
        )
        .await;
        tokio::time::timeout(Duration::from_secs(2), dropped.notified())
            .await
            .expect("a cancelled upgrade must release its in-process router task");
        harness.cancel_tx.send(true).unwrap();
        harness.join.await.unwrap();
    }

    #[tokio::test]
    async fn sse_streams_before_completion_and_does_not_block_other_requests() {
        use axum::response::sse::{Event, Sse};
        let Some((url, mut server_rx)) = fake_cloud_server().await else {
            return;
        };
        let harness = Harness::start(&url).await;
        let (event_tx, event_rx) = mpsc::channel::<Result<Event, std::convert::Infallible>>(2);
        let event_rx = Arc::new(tokio::sync::Mutex::new(Some(event_rx)));
        harness
            .set_router(
                Router::new()
                    .route(
                        "/events",
                        get(move || {
                            let event_rx = event_rx.clone();
                            async move {
                                let rx = event_rx.lock().await.take().unwrap();
                                Sse::new(futures_util::stream::unfold(rx, |mut rx| async move {
                                    rx.recv().await.map(|item| (item, rx))
                                }))
                            }
                        }),
                    )
                    .route("/fast", get(|| async { "ok" })),
            )
            .await;
        let mut cloud = server_rx.recv().await.unwrap();
        cloud_handshake(&mut cloud).await;
        send_oidc_config(&mut cloud).await;
        let open = open_request(Uuid::new_v4(), "GET", "/events");
        send_control(&mut cloud, ConsoleStreamOpen::KIND, &open).await;
        let head: ConsoleResponseHead = recv_control(&mut cloud)
            .await
            .decode(ConsoleResponseHead::KIND)
            .unwrap();
        assert_eq!(head.status, 200);
        assert_eq!(
            header_value(&head.headers, "content-type"),
            Some("text/event-stream")
        );
        for value in ["first", "second"] {
            event_tx
                .send(Ok(Event::default().data(value)))
                .await
                .unwrap();
            let frame = tokio::time::timeout(Duration::from_secs(1), recv_wire_frame(&mut cloud))
                .await
                .unwrap()
                .unwrap();
            match frame {
                WireFrame::Data(frame) => {
                    assert_eq!(frame.stream_id, open.stream_id);
                    assert!(std::str::from_utf8(frame.payload())
                        .unwrap()
                        .contains(value));
                }
                _ => panic!("event must arrive while its source remains open"),
            }
        }
        let next = open_request(Uuid::new_v4(), "GET", "/fast");
        send_control(&mut cloud, ConsoleStreamOpen::KIND, &next).await;
        assert_eq!(expect_response(&mut cloud, next.stream_id).await.1, b"ok");
        drop(event_tx);
        let end: ConsoleStreamEnd = recv_control(&mut cloud)
            .await
            .decode(ConsoleStreamEnd::KIND)
            .unwrap();
        assert_eq!(end.stream_id, open.stream_id);
        assert_eq!(end.reason, ConsoleStreamEndReason::Complete);
        harness.cancel_tx.send(true).unwrap();
        harness.join.await.unwrap();
    }

    #[tokio::test]
    async fn a_chunked_post_body_arrives_whole_with_window_updates() {
        let Some((backend_url, mut server_rx)) = fake_cloud_server().await else {
            return;
        };
        let harness = Harness::start(&backend_url).await;
        harness
            .set_router(axum::Router::new().route(
                "/echo",
                axum::routing::post(|body: Bytes| async move { body }),
            ))
            .await;

        let mut cloud = server_rx.recv().await.expect("worker must connect");
        cloud_handshake(&mut cloud).await;
        send_oidc_config(&mut cloud).await;

        let payload = b"the-quick-brown-fox-jumps-over-the-lazy-dog".repeat(50);
        let stream_id = test_uuid(3);
        let mut open = open_request(stream_id, "POST", "/echo");
        open.headers
            .push(("content-length".into(), payload.len().to_string()));
        send_control(&mut cloud, ConsoleStreamOpen::KIND, &open).await;

        // Send the body split across several frames, well under one frame's
        // max payload, and confirm the instance replenishes credit for each.
        for chunk in payload.chunks(500) {
            send_data(
                &mut cloud,
                ConsoleFrameKind::RequestBodyChunk,
                stream_id,
                Bytes::copy_from_slice(chunk),
            )
            .await;
            let envelope = recv_control(&mut cloud).await;
            assert_eq!(envelope.kind, ConsoleWindowUpdate::KIND);
            let update: ConsoleWindowUpdate = envelope
                .decode(ConsoleWindowUpdate::KIND)
                .expect("window update must decode");
            assert_eq!(update.stream_id, stream_id);
            assert_eq!(update.additional_bytes as usize, chunk.len());
        }

        let (head, body) = expect_response(&mut cloud, stream_id).await;
        assert_eq!(head.status, 200);
        assert_eq!(body, payload);

        harness.cancel_tx.send(true).expect("cancel must send");
        let _ = tokio::time::timeout(Duration::from_secs(5), harness.join).await;
    }

    #[tokio::test]
    async fn a_stream_opened_before_oidc_config_is_refused_not_configured() {
        let Some((backend_url, mut server_rx)) = fake_cloud_server().await else {
            return;
        };
        let harness = Harness::start(&backend_url).await;

        let mut cloud = server_rx.recv().await.expect("worker must connect");
        cloud_handshake(&mut cloud).await;
        // Deliberately no `send_oidc_config` — the exact fail-closed race
        // window ADR-045 §3 requires.

        let stream_id = test_uuid(4);
        send_control(
            &mut cloud,
            ConsoleStreamOpen::KIND,
            &open_request(stream_id, "GET", "/hello"),
        )
        .await;

        let envelope = recv_control(&mut cloud).await;
        assert_eq!(envelope.kind, ConsoleStreamRefused::KIND);
        let refused: ConsoleStreamRefused = envelope
            .decode(ConsoleStreamRefused::KIND)
            .expect("refusal must decode");
        assert_eq!(refused.reason, ConsoleRefusalReason::NotConfigured);

        harness.cancel_tx.send(true).expect("cancel must send");
        let _ = tokio::time::timeout(Duration::from_secs(5), harness.join).await;
    }

    #[tokio::test]
    async fn a_host_mismatch_is_refused_before_the_router_is_called() {
        let Some((backend_url, mut server_rx)) = fake_cloud_server().await else {
            return;
        };
        let harness = Harness::start(&backend_url).await;
        async fn never_called_host() -> &'static str {
            panic!("the router must never be called on a Host mismatch")
        }
        harness
            .set_router(axum::Router::new().route("/hello", get(never_called_host)))
            .await;

        let mut cloud = server_rx.recv().await.expect("worker must connect");
        cloud_handshake(&mut cloud).await;
        send_oidc_config(&mut cloud).await;

        let stream_id = test_uuid(5);
        let mut open = open_request(stream_id, "GET", "/hello");
        open.headers = vec![("host".into(), "not-the-pinned-host.invalid".into())];
        send_control(&mut cloud, ConsoleStreamOpen::KIND, &open).await;

        let envelope = recv_control(&mut cloud).await;
        assert_eq!(envelope.kind, ConsoleStreamRefused::KIND);
        let refused: ConsoleStreamRefused = envelope
            .decode(ConsoleStreamRefused::KIND)
            .expect("refusal must decode");
        assert_eq!(refused.reason, ConsoleRefusalReason::HostMismatch);

        harness.cancel_tx.send(true).expect("cancel must send");
        let _ = tokio::time::timeout(Duration::from_secs(5), harness.join).await;
    }

    /// A `ConsoleStreamOpen` that reuses the `stream_id` of an already-open
    /// stream must never overwrite that stream's table entry — the original
    /// stream keeps running (proven by its response still arriving
    /// correctly), and the duplicate is refused outright.
    #[tokio::test]
    async fn a_duplicate_stream_id_is_refused_and_the_original_keeps_running() {
        let Some((backend_url, mut server_rx)) = fake_cloud_server().await else {
            return;
        };
        let harness = Harness::start(&backend_url).await;
        harness
            .set_router(
                axum::Router::new()
                    .route(
                        "/hang",
                        get(|| async {
                            std::future::pending::<()>().await;
                            "never"
                        }),
                    )
                    .route("/hello", get(|| async { "hi" })),
            )
            .await;

        let mut cloud = server_rx.recv().await.expect("worker must connect");
        cloud_handshake(&mut cloud).await;
        send_oidc_config(&mut cloud).await;

        let stream_id = test_uuid(12);
        send_control(
            &mut cloud,
            ConsoleStreamOpen::KIND,
            &open_request(stream_id, "GET", "/hang"),
        )
        .await;
        tokio::time::sleep(Duration::from_millis(200)).await;

        // A malformed duplicate must not emit an end for the original stream.
        for oversized in [false, true] {
            let mut duplicate = open_request(stream_id, "GET", "/hello");
            if oversized {
                duplicate
                    .headers
                    .push(("x-oversized".into(), "x".repeat(CONSOLE_MAX_HEADER_BYTES)));
            }
            send_control(&mut cloud, ConsoleStreamOpen::KIND, &duplicate).await;
            let envelope = recv_control(&mut cloud).await;
            assert_eq!(envelope.kind, ConsoleStreamRefused::KIND);
            let refused: ConsoleStreamRefused =
                envelope.decode(ConsoleStreamRefused::KIND).unwrap();
            assert_eq!(refused.stream_id, stream_id);
            assert_eq!(refused.reason, ConsoleRefusalReason::DuplicateStream);
        }

        // The original hanging stream must still be the one occupying that
        // id — cancel it and confirm the slot only frees up now, proving the
        // duplicate never touched its table entry.
        send_control(
            &mut cloud,
            ConsoleStreamCancel::KIND,
            &ConsoleStreamCancel { stream_id },
        )
        .await;
        tokio::time::sleep(Duration::from_millis(200)).await;

        let retry_id = test_uuid(13);
        send_control(
            &mut cloud,
            ConsoleStreamOpen::KIND,
            &open_request(retry_id, "GET", "/hello"),
        )
        .await;
        let (head, body) = expect_response(&mut cloud, retry_id).await;
        assert_eq!(head.status, 200);
        assert_eq!(String::from_utf8(body).unwrap(), "hi");

        harness.cancel_tx.send(true).expect("cancel must send");
        let _ = tokio::time::timeout(Duration::from_secs(5), harness.join).await;
    }

    #[tokio::test]
    async fn an_upgrade_with_the_wrong_origin_is_refused_before_the_router_is_called() {
        let Some((backend_url, mut server_rx)) = fake_cloud_server().await else {
            return;
        };
        let harness = Harness::start(&backend_url).await;
        async fn never_called_origin() -> &'static str {
            panic!("the router must never be called on an Origin mismatch")
        }
        harness
            .set_router(axum::Router::new().route("/ws", get(never_called_origin)))
            .await;

        let mut cloud = server_rx.recv().await.expect("worker must connect");
        cloud_handshake(&mut cloud).await;
        send_oidc_config(&mut cloud).await;

        let stream_id = test_uuid(6);
        let mut open = open_request(stream_id, "GET", "/ws");
        open.upgrade_requested = true;
        open.headers
            .push(("origin".into(), "https://not-the-console.invalid".into()));
        send_control(&mut cloud, ConsoleStreamOpen::KIND, &open).await;

        let envelope = recv_control(&mut cloud).await;
        assert_eq!(envelope.kind, ConsoleStreamRefused::KIND);
        let refused: ConsoleStreamRefused = envelope
            .decode(ConsoleStreamRefused::KIND)
            .expect("refusal must decode");
        assert_eq!(refused.reason, ConsoleRefusalReason::OriginMismatch);

        harness.cancel_tx.send(true).expect("cancel must send");
        let _ = tokio::time::timeout(Duration::from_secs(5), harness.join).await;
    }

    #[tokio::test]
    async fn a_17th_concurrent_stream_is_refused_and_cancelling_one_frees_a_slot() {
        let Some((backend_url, mut server_rx)) = fake_cloud_server().await else {
            return;
        };
        let harness = Harness::start(&backend_url).await;
        harness
            .set_router(
                axum::Router::new()
                    .route(
                        "/hang",
                        get(|| async {
                            std::future::pending::<()>().await;
                            "never"
                        }),
                    )
                    .route("/hello", get(|| async { "hi" })),
            )
            .await;

        let mut cloud = server_rx.recv().await.expect("worker must connect");
        cloud_handshake(&mut cloud).await;
        send_oidc_config(&mut cloud).await;

        let mut hanging_ids = Vec::new();
        for i in 0..CONSOLE_MAX_CONCURRENT_STREAMS {
            let stream_id = Uuid::from_u128(0x1000_0000 + i as u128);
            hanging_ids.push(stream_id);
            send_control(
                &mut cloud,
                ConsoleStreamOpen::KIND,
                &open_request(stream_id, "GET", "/hang"),
            )
            .await;
        }
        // Give every hanging stream's task a moment to register itself in
        // the shared table before probing the limit.
        tokio::time::sleep(Duration::from_millis(200)).await;

        let overflow_id = test_uuid(7);
        send_control(
            &mut cloud,
            ConsoleStreamOpen::KIND,
            &open_request(overflow_id, "GET", "/hello"),
        )
        .await;
        let envelope = recv_control(&mut cloud).await;
        assert_eq!(envelope.kind, ConsoleStreamRefused::KIND);
        let refused: ConsoleStreamRefused = envelope
            .decode(ConsoleStreamRefused::KIND)
            .expect("refusal must decode");
        assert_eq!(refused.reason, ConsoleRefusalReason::TooManyStreams);

        // Cancel one of the hanging streams, freeing a slot.
        let cancelled_id = hanging_ids[0];
        send_control(
            &mut cloud,
            ConsoleStreamCancel::KIND,
            &ConsoleStreamCancel {
                stream_id: cancelled_id,
            },
        )
        .await;
        tokio::time::sleep(Duration::from_millis(200)).await;

        let retry_id = test_uuid(8);
        send_control(
            &mut cloud,
            ConsoleStreamOpen::KIND,
            &open_request(retry_id, "GET", "/hello"),
        )
        .await;
        let (head, body) = expect_response(&mut cloud, retry_id).await;
        assert_eq!(head.status, 200);
        assert_eq!(String::from_utf8(body).unwrap(), "hi");

        harness.cancel_tx.send(true).expect("cancel must send");
        let _ = tokio::time::timeout(Duration::from_secs(5), harness.join).await;
    }

    #[tokio::test]
    async fn shutdown_sends_going_away_on_every_open_stream() {
        let Some((backend_url, mut server_rx)) = fake_cloud_server().await else {
            return;
        };
        let harness = Harness::start(&backend_url).await;
        harness
            .set_router(axum::Router::new().route(
                "/hang",
                get(|| async {
                    std::future::pending::<()>().await;
                    "never"
                }),
            ))
            .await;

        let mut cloud = server_rx.recv().await.expect("worker must connect");
        cloud_handshake(&mut cloud).await;
        send_oidc_config(&mut cloud).await;

        let stream_id = test_uuid(9);
        send_control(
            &mut cloud,
            ConsoleStreamOpen::KIND,
            &open_request(stream_id, "GET", "/hang"),
        )
        .await;
        tokio::time::sleep(Duration::from_millis(200)).await;

        harness.cancel_tx.send(true).expect("cancel must send");

        let envelope = recv_control(&mut cloud).await;
        assert_eq!(envelope.kind, ConsoleStreamEnd::KIND);
        let end: ConsoleStreamEnd = envelope
            .decode(ConsoleStreamEnd::KIND)
            .expect("stream end must decode");
        assert_eq!(end.stream_id, stream_id);
        assert_eq!(end.reason, ConsoleStreamEndReason::GoingAway);

        let _ = tokio::time::timeout(Duration::from_secs(5), harness.join).await;
    }

    /// Flipping `cloud.console_access_enabled` off mid-connection must close
    /// the console-proxy connection the same way a shutdown does — every
    /// open stream gets `GoingAway` before the socket goes down — rather than
    /// leaving a stream to time out on its own.
    #[tokio::test]
    async fn disabling_mid_connection_ends_open_streams_with_going_away() {
        let Some((backend_url, mut server_rx)) = fake_cloud_server().await else {
            return;
        };
        let harness = Harness::start(&backend_url).await;
        harness
            .set_router(axum::Router::new().route(
                "/hang",
                get(|| async {
                    std::future::pending::<()>().await;
                    "never"
                }),
            ))
            .await;

        let mut cloud = server_rx.recv().await.expect("worker must connect");
        cloud_handshake(&mut cloud).await;
        send_oidc_config(&mut cloud).await;

        let stream_id = test_uuid(11);
        send_control(
            &mut cloud,
            ConsoleStreamOpen::KIND,
            &open_request(stream_id, "GET", "/hang"),
        )
        .await;
        tokio::time::sleep(Duration::from_millis(200)).await;

        harness
            .enabled_tx
            .send(false)
            .expect("enabled toggle must send");

        let envelope = recv_control(&mut cloud).await;
        assert_eq!(envelope.kind, ConsoleStreamEnd::KIND);
        let end: ConsoleStreamEnd = envelope
            .decode(ConsoleStreamEnd::KIND)
            .expect("stream end must decode");
        assert_eq!(end.stream_id, stream_id);
        assert_eq!(end.reason, ConsoleStreamEndReason::GoingAway);

        harness.cancel_tx.send(true).expect("cancel must send");
        let _ = tokio::time::timeout(Duration::from_secs(5), harness.join).await;
    }

    // -----------------------------------------------------------------
    // WebSocket upgrade relay
    // -----------------------------------------------------------------

    /// Adapts the [`ConsoleFrameKind::WsRelay`] byte exchange with a fake
    /// Cloud into an `AsyncRead + AsyncWrite`, so the test can speak genuine
    /// RFC 6455 framing (via [`tokio_tungstenite::WebSocketStream::from_raw_socket`])
    /// over the relayed pipe instead of hand-rolling frame masking.
    struct RelayIo {
        outgoing: mpsc::Sender<bytes::Bytes>,
        incoming: mpsc::Receiver<bytes::Bytes>,
        leftover: bytes::Bytes,
    }

    impl tokio::io::AsyncRead for RelayIo {
        fn poll_read(
            self: Pin<&mut Self>,
            cx: &mut TaskContext<'_>,
            buf: &mut tokio::io::ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            let this = self.get_mut();
            if this.leftover.is_empty() {
                match this.incoming.poll_recv(cx) {
                    Poll::Ready(Some(bytes)) => this.leftover = bytes,
                    Poll::Ready(None) => return Poll::Ready(Ok(())),
                    Poll::Pending => return Poll::Pending,
                }
            }
            let take = this.leftover.len().min(buf.remaining());
            let chunk = this.leftover.split_to(take);
            buf.put_slice(&chunk);
            Poll::Ready(Ok(()))
        }
    }

    impl tokio::io::AsyncWrite for RelayIo {
        fn poll_write(
            self: Pin<&mut Self>,
            cx: &mut TaskContext<'_>,
            buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            let len = buf.len().min(16 * 1024);
            match self
                .outgoing
                .try_send(bytes::Bytes::copy_from_slice(&buf[..len]))
            {
                Ok(()) => Poll::Ready(Ok(len)),
                Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                    cx.waker().wake_by_ref();
                    Poll::Pending
                }
                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => Poll::Ready(Err(
                    io::Error::new(io::ErrorKind::BrokenPipe, "relay closed"),
                )),
            }
        }
        fn poll_flush(self: Pin<&mut Self>, _cx: &mut TaskContext<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut TaskContext<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    async fn echo_ws(ws: WebSocketUpgrade) -> impl IntoResponse {
        ws.on_upgrade(|mut socket| async move {
            // Write more than the in-process duplex capacity before reading.
            // Both peers writing first must not deadlock the relay.
            socket
                .send(AxumMessage::Binary(Bytes::from(vec![7; 128 * 1024])))
                .await
                .expect("server banner sends");
            while let Some(Ok(message)) = socket.recv().await {
                if matches!(message, AxumMessage::Close(_)) {
                    break;
                }
                if socket.send(message).await.is_err() {
                    break;
                }
            }
        })
    }

    #[tokio::test]
    async fn a_websocket_route_is_relayed_both_ways() {
        let Some((backend_url, mut server_rx)) = fake_cloud_server().await else {
            return;
        };
        let harness = Harness::start(&backend_url).await;
        harness
            .set_router(axum::Router::new().route("/ws", get(echo_ws)))
            .await;

        let mut cloud = server_rx.recv().await.expect("worker must connect");
        cloud_handshake(&mut cloud).await;
        send_oidc_config(&mut cloud).await;

        let stream_id = test_uuid(10);
        let mut open = open_request(stream_id, "GET", "/ws");
        open.upgrade_requested = true;
        open.headers
            .push(("origin".into(), format!("https://{PINNED_HOST}")));
        open.headers.push(("connection".into(), "Upgrade".into()));
        open.headers.push(("upgrade".into(), "websocket".into()));
        open.headers.push((
            "sec-websocket-key".into(),
            "dGhlIHNhbXBsZSBub25jZQ==".into(),
        ));
        open.headers
            .push(("sec-websocket-version".into(), "13".into()));
        send_control(&mut cloud, ConsoleStreamOpen::KIND, &open).await;

        let envelope = recv_control(&mut cloud).await;
        assert_eq!(envelope.kind, ConsoleResponseHead::KIND);
        let head: ConsoleResponseHead = envelope
            .decode(ConsoleResponseHead::KIND)
            .expect("response head must decode");
        assert_eq!(head.status, StatusCode::SWITCHING_PROTOCOLS.as_u16());
        assert!(head.upgraded);

        // From here, `cloud` carries only `WsRelay` data frames for this
        // stream; pump them through a `RelayIo` so a real tungstenite
        // `WebSocketStream` (client role, handshake already done) can speak
        // genuine RFC 6455 framing over the relay.
        let (to_instance_tx, mut to_instance_rx) = mpsc::channel::<bytes::Bytes>(64);
        let (from_instance_tx, from_instance_rx) = mpsc::channel::<bytes::Bytes>(64);
        let pump = tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    outgoing = to_instance_rx.recv() => {
                        match outgoing {
                            Some(bytes) => {
                                send_data(&mut cloud, ConsoleFrameKind::WsRelay, stream_id, bytes).await;
                            }
                            None => break,
                        }
                    }
                    frame = recv_wire_frame(&mut cloud) => {
                        match frame {
                            Some(WireFrame::Data(frame)) if frame.frame_kind == ConsoleFrameKind::WsRelay => {
                                if from_instance_tx.send(frame.payload().clone()).await.is_err() {
                                    break;
                                }
                                send_control(&mut cloud, ConsoleWindowUpdate::KIND, &ConsoleWindowUpdate { stream_id, additional_bytes: frame.payload().len() as u32 }).await;
                            }
                            Some(_) => continue,
                            None => break,
                        }
                    }
                }
            }
        });

        let relay_io = RelayIo {
            outgoing: to_instance_tx,
            incoming: from_instance_rx,
            leftover: bytes::Bytes::new(),
        };
        let mut ws = WebSocketStream::from_raw_socket(relay_io, Role::Client, None).await;

        let large_message = Bytes::from(vec![9; 128 * 1024]);
        tokio::time::timeout(
            Duration::from_secs(5),
            ws.send(Message::Binary(large_message.clone())),
        )
        .await
        .expect("large client write must not deadlock")
        .expect("client binary sends");
        let banner = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("server write must progress independently")
            .expect("banner arrives")
            .expect("banner readable");
        assert_eq!(banner.into_data(), Bytes::from(vec![7; 128 * 1024]));
        let large_echo = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("large echo must arrive")
            .expect("echo arrives")
            .expect("echo readable");
        assert_eq!(large_echo.into_data(), large_message);

        ws.send(Message::Text("hello over the tunnel".into()))
            .await
            .expect("client frame must send");
        let echoed = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("echo must arrive promptly")
            .expect("stream must not end")
            .expect("frame must be readable");
        assert_eq!(
            echoed.into_text().expect("text frame"),
            "hello over the tunnel"
        );

        drop(ws);
        pump.abort();
        harness.cancel_tx.send(true).expect("cancel must send");
        let _ = tokio::time::timeout(Duration::from_secs(5), harness.join).await;
    }
}
