// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Liveness signal sent to the managed backend's dedicated heartbeat channel.
//!
//! # Why this exists
//!
//! Telemetry shipment and backup mirroring only ever run when there is
//! something to ship — an instance with no traffic and no completed backups
//! never touches the Cloud backend at all, so the console has no way to tell
//! "linked and quiet" apart from "linked and unreachable". This task closes
//! that gap: it holds the one WebSocket connection dedicated to proving this
//! instance is up, independent of whatever else it does or does not have to
//! say.
//!
//! # The rule this module keeps like every other one in this crate
//!
//! **Local is primary.** A dead or degraded management channel must never
//! slow, block, or fail anything else the instance does. A connection
//! attempt that fails is logged at debug level and retried on a bounded,
//! exponential backoff — never a tight loop, never a panic, never a blocking
//! call on any other path.
//!
//! # Protocol
//!
//! `GET {backend}/v1/management`, upgraded to a WebSocket and authenticated
//! with this instance's linked bearer token, same as every other Cloud call.
//! The server sends [`Hello`] first; this task must reply with its own
//! `Hello` inside the server's handshake window (comfortably covered by
//! [`HANDSHAKE_TIMEOUT`]) or the server closes the connection. Once
//! negotiated, this task sends a [`Heartbeat`] envelope every
//! [`HEARTBEAT_INTERVAL`] — comfortably under the server's own idle
//! timeout — and reads back a `heartbeat_ack` envelope carrying
//! [`HeartbeatAck`] for skew diagnostics only; it is never used for a local
//! authorization or billing decision, matching the wire type's own contract.

use std::sync::Arc;
use std::time::Duration;

use futures_util::{stream::SplitSink, stream::SplitStream, SinkExt, StreamExt};
use tokio::sync::watch;
use tokio_tungstenite::{
    tungstenite::{client::IntoClientRequest, http::header::AUTHORIZATION, Message},
    MaybeTlsStream, WebSocketStream,
};
use uuid::Uuid;

use temps_cloud_protocol::{
    Capability, Envelope, Heartbeat, HeartbeatAck, Hello, PROTOCOL_VERSION,
};

use crate::link::CloudLink;
use crate::BackendUrl;

type WsStream = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;
type WsWrite = SplitSink<WsStream, Message>;
type WsRead = SplitStream<WsStream>;

/// Cadence for a healthy connection. Comfortably under the server's
/// heartbeat-idle timeout (90s), matching the interval called out in the
/// protocol design as the common, safe choice.
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);

/// Bound on connect + handshake. The server closes the connection if it does
/// not receive this task's `Hello` within its own 10s window, so completing
/// well inside that (rather than racing it exactly) leaves margin for a slow
/// TLS handshake on a loaded instance.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(8);

/// Outage ceiling for reconnect backoff. An unreachable or unlinked backend
/// must never be hammered, but this task keeps ticking at this rate forever
/// so recovery — or enrollment — is noticed without a restart.
const MAX_RECONNECT_INTERVAL: Duration = Duration::from_secs(15 * 60);

/// What a connection cycle did, so the caller knows how to schedule the next
/// attempt. Mirrors the shape of `temps-cloud::backup_mirror`'s
/// `SweepOutcome`/`next_sweep_interval`: a fresh instance starts at the base
/// interval, every subsequent failure doubles it up to a ceiling, and only an
/// explicit shutdown request skips the requeue entirely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CycleOutcome {
    /// Not linked, or no credential yet. No network attempted.
    NotLinked,
    /// A connection attempt, the handshake, or an established connection
    /// ended (or never started) for a reason retrying can plausibly fix.
    Disconnected,
    /// Shutdown was requested. The caller must stop, not reconnect.
    Cancelled,
}

fn next_reconnect_interval(current: Duration, outcome: CycleOutcome) -> Duration {
    match outcome {
        CycleOutcome::Cancelled => Duration::ZERO,
        CycleOutcome::NotLinked | CycleOutcome::Disconnected if current.is_zero() => {
            HEARTBEAT_INTERVAL
        }
        CycleOutcome::NotLinked | CycleOutcome::Disconnected => {
            (current * 2).min(MAX_RECONNECT_INTERVAL)
        }
    }
}

/// Run until cancelled. Spawn this once at instance startup, alongside the
/// backup mirror and the telemetry flusher — it self-gates on
/// [`CloudLink::is_linked`] exactly like the backup mirror does, so it is
/// safe to spawn unconditionally and it starts working the moment the
/// instance links, with no separate start/stop wiring required.
pub async fn run(link: Arc<CloudLink>, mut cancel: watch::Receiver<bool>) {
    tracing::info!("Cloud heartbeat sender started");
    let mut retry_in = Duration::ZERO;
    loop {
        tokio::select! {
            biased;
            changed = cancel.changed() => {
                if changed.is_err() || *cancel.borrow() {
                    tracing::info!("Cloud heartbeat sender stopped after shutdown request");
                    return;
                }
            }
            _ = tokio::time::sleep(retry_in) => {
                if !link.is_linked() {
                    tracing::debug!("Cloud heartbeat sender has nothing to do: instance is not linked");
                    retry_in = next_reconnect_interval(retry_in, CycleOutcome::NotLinked);
                    continue;
                }
                tracing::debug!("Cloud heartbeat connection cycle starting");
                let outcome = connection_cycle(&link, &mut cancel).await;
                tracing::debug!(outcome = ?outcome, "Cloud heartbeat connection cycle finished");
                if outcome == CycleOutcome::Cancelled {
                    tracing::info!("Cloud heartbeat sender stopped after shutdown request");
                    return;
                }
                retry_in = next_reconnect_interval(retry_in, outcome);
            }
        }
    }
}

/// Connect, negotiate, then send heartbeats until the connection drops or
/// shutdown is requested.
async fn connection_cycle(
    link: &Arc<CloudLink>,
    cancel: &mut watch::Receiver<bool>,
) -> CycleOutcome {
    let (base_url, token) = match link.linked_credential() {
        Ok(credential) => credential,
        Err(error) => {
            tracing::debug!(%error, "Cloud heartbeat sender has no linked credential");
            return CycleOutcome::NotLinked;
        }
    };
    let Some(instance_id) = link.instance_id() else {
        tracing::debug!("Cloud heartbeat sender has no instance id yet");
        return CycleOutcome::NotLinked;
    };
    let backend = match link.parse_backend(&base_url) {
        Ok(backend) => backend,
        Err(error) => {
            tracing::warn!(%error, "Cloud heartbeat sender could not parse the managed backend URL");
            return CycleOutcome::Disconnected;
        }
    };

    let (mut write, mut read) = match connect(&backend, &token).await {
        Ok(streams) => streams,
        Err(error) => {
            tracing::debug!(
                %error,
                "Cloud heartbeat connection failed; local operation is unaffected"
            );
            return CycleOutcome::Disconnected;
        }
    };

    if let Err(error) = handshake(link, &mut write, &mut read).await {
        tracing::warn!(%error, "Cloud heartbeat handshake failed; will retry");
        let _ = write.close().await;
        return CycleOutcome::Disconnected;
    }
    tracing::info!("Cloud heartbeat connection established and negotiated");

    heartbeat_loop(link, instance_id, &mut write, &mut read, cancel).await
}

async fn connect(backend: &BackendUrl, token: &str) -> Result<(WsWrite, WsRead), HeartbeatError> {
    let url = management_ws_url(backend)?;
    let mut request =
        url.as_str()
            .into_client_request()
            .map_err(|error| HeartbeatError::InvalidRequest {
                reason: error.to_string(),
            })?;
    let authorization = format!("Bearer {token}")
        .parse::<tokio_tungstenite::tungstenite::http::HeaderValue>()
        .map_err(|error| HeartbeatError::InvalidRequest {
            reason: error.to_string(),
        })?;
    request.headers_mut().insert(AUTHORIZATION, authorization);

    let (stream, _response) = tokio_tungstenite::connect_async(request)
        .await
        .map_err(|source| HeartbeatError::Connect { source })?;
    let (write, read) = stream.split();
    Ok((write, read))
}

/// `wss://{host}/v1/management`, or `ws://` for an explicit loopback
/// development backend — the same origin every other Cloud call already
/// targets, just upgraded to a WebSocket.
fn management_ws_url(backend: &BackendUrl) -> Result<url::Url, HeartbeatError> {
    let mut url = backend.endpoint("/v1/management");
    let scheme = match url.scheme() {
        "https" => "wss",
        "http" => "ws",
        other => {
            return Err(HeartbeatError::InvalidRequest {
                reason: format!("unsupported scheme {other:?} for the management channel"),
            })
        }
    };
    url.set_scheme(scheme)
        .map_err(|()| HeartbeatError::InvalidRequest {
            reason: format!("could not switch the management endpoint to the {scheme} scheme"),
        })?;
    Ok(url)
}

/// Read the server's opening [`Hello`] and reply with our own, within
/// [`HANDSHAKE_TIMEOUT`].
async fn handshake(
    link: &CloudLink,
    write: &mut WsWrite,
    read: &mut WsRead,
) -> Result<(), HeartbeatError> {
    tokio::time::timeout(HANDSHAKE_TIMEOUT, read.next())
        .await
        .map_err(|_| HeartbeatError::HelloTimeout)?
        .ok_or(HeartbeatError::ConnectionClosedDuringHandshake)?
        .map_err(|source| HeartbeatError::Connect { source })
        .and_then(|message| decode_hello(&message))?;

    let hello = Hello {
        protocol_version: PROTOCOL_VERSION,
        agent_version: link.agent_version().to_string(),
        capabilities: vec![
            Capability::TelemetryShipping,
            Capability::BackupOrchestration,
            Capability::ManagedAiInference,
        ],
    };
    send_envelope(write, "hello", &hello).await
}

/// Send heartbeats on [`HEARTBEAT_INTERVAL`] and read back acknowledgements
/// until the connection ends or shutdown is requested.
async fn heartbeat_loop(
    link: &CloudLink,
    instance_id: Uuid,
    write: &mut WsWrite,
    read: &mut WsRead,
    cancel: &mut watch::Receiver<bool>,
) -> CycleOutcome {
    let mut ticker = tokio::time::interval(HEARTBEAT_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // `interval`'s own first tick fires immediately (deadline == creation
    // time), so the loop's first iteration below sends the first heartbeat
    // right away rather than waiting a full interval -- the connection just
    // negotiated, and that is what lets a freshly linked instance clear
    // "awaiting signal" without an extra wait.

    loop {
        tokio::select! {
            biased;
            changed = cancel.changed() => {
                if changed.is_err() || *cancel.borrow() {
                    let _ = write.close().await;
                    return CycleOutcome::Cancelled;
                }
            }
            _ = ticker.tick() => {
                let heartbeat = Heartbeat {
                    instance_id,
                    public_ip: None,
                    country_code: None,
                    region: None,
                    city: None,
                    pending_spool_bytes: link.spooled_bytes(),
                };
                if let Err(error) = send_envelope(write, "heartbeat", &heartbeat).await {
                    tracing::debug!(%error, "Cloud heartbeat send failed; will reconnect");
                    return CycleOutcome::Disconnected;
                }
                tracing::debug!(
                    pending_spool_bytes = heartbeat.pending_spool_bytes,
                    "Cloud heartbeat sent"
                );
            }
            message = read.next() => {
                match message {
                    Some(Ok(message)) if is_close(&message) => {
                        tracing::debug!("Cloud heartbeat connection closed by the backend");
                        return CycleOutcome::Disconnected;
                    }
                    Some(Ok(message)) => {
                        if let Some(ack) = decode_heartbeat_ack(&message) {
                            // Skew diagnostics only, per HeartbeatAck's own
                            // doc comment -- never a local authorization or
                            // billing input.
                            tracing::debug!(
                                received_at_millis = ack.received_at_millis,
                                "Cloud heartbeat acknowledged"
                            );
                        }
                        // Any other frame kind (a future addition, a ping the
                        // client library already answered) is ignored: this
                        // channel's only obligation is to keep heartbeats
                        // flowing.
                    }
                    Some(Err(error)) => {
                        tracing::debug!(%error, "Cloud heartbeat connection error; will reconnect");
                        return CycleOutcome::Disconnected;
                    }
                    None => {
                        tracing::debug!("Cloud heartbeat connection closed");
                        return CycleOutcome::Disconnected;
                    }
                }
            }
        }
    }
}

async fn send_envelope<T: serde::Serialize>(
    write: &mut WsWrite,
    kind: &str,
    payload: &T,
) -> Result<(), HeartbeatError> {
    let envelope =
        Envelope::new(kind, payload).map_err(|source| HeartbeatError::Encode { source })?;
    let text =
        serde_json::to_string(&envelope).map_err(|source| HeartbeatError::Encode { source })?;
    write
        .send(Message::Text(text.into()))
        .await
        .map_err(|source| HeartbeatError::Send { source })
}

fn decode_envelope(message: &Message) -> Option<Envelope> {
    let text = message.to_text().ok()?;
    serde_json::from_str(text).ok()
}

fn decode_hello(message: &Message) -> Result<Hello, HeartbeatError> {
    let envelope = decode_envelope(message).ok_or(HeartbeatError::InvalidServerHello {
        reason: "the first frame was not a readable envelope".to_string(),
    })?;
    let kind = envelope.kind.clone();
    envelope
        .decode::<Hello>("hello")
        .ok_or(HeartbeatError::InvalidServerHello {
            reason: format!("expected a hello envelope, got kind {kind:?}"),
        })
}

fn decode_heartbeat_ack(message: &Message) -> Option<HeartbeatAck> {
    decode_envelope(message)?.decode::<HeartbeatAck>("heartbeat_ack")
}

fn is_close(message: &Message) -> bool {
    matches!(message, Message::Close(_))
}

#[derive(Debug, thiserror::Error)]
enum HeartbeatError {
    #[error("could not build the management channel request: {reason}")]
    InvalidRequest { reason: String },
    #[error("could not connect to the Cloud management channel: {source}")]
    Connect {
        #[source]
        source: tokio_tungstenite::tungstenite::Error,
    },
    #[error("could not encode a management envelope: {source}")]
    Encode {
        #[source]
        source: serde_json::Error,
    },
    #[error("could not send a management frame: {source}")]
    Send {
        #[source]
        source: tokio_tungstenite::tungstenite::Error,
    },
    #[error("the server hello was unreadable: {reason}")]
    InvalidServerHello { reason: String },
    #[error("timed out waiting for the server hello")]
    HelloTimeout,
    #[error("the connection closed before the server sent its hello")]
    ConnectionClosedDuringHandshake,
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

    use axum::{
        extract::{
            ws::{Message as AxumMessage, WebSocket, WebSocketUpgrade},
            State,
        },
        response::IntoResponse,
        routing::get,
        Router,
    };

    use super::*;

    #[test]
    fn a_disconnect_backs_off_and_is_capped() {
        let mut d = Duration::ZERO;
        d = next_reconnect_interval(d, CycleOutcome::Disconnected);
        assert_eq!(d, HEARTBEAT_INTERVAL);
        for _ in 0..20 {
            d = next_reconnect_interval(d, CycleOutcome::Disconnected);
        }
        assert_eq!(d, MAX_RECONNECT_INTERVAL, "backoff must be bounded");
    }

    #[test]
    fn an_unlinked_instance_still_ticks_but_slows_down() {
        let mut d = Duration::ZERO;
        d = next_reconnect_interval(d, CycleOutcome::NotLinked);
        assert_eq!(d, HEARTBEAT_INTERVAL);
        d = next_reconnect_interval(d, CycleOutcome::NotLinked);
        assert!(d > HEARTBEAT_INTERVAL);
        assert!(d <= MAX_RECONNECT_INTERVAL);
    }

    #[test]
    fn cancellation_never_schedules_a_reconnect() {
        assert_eq!(
            next_reconnect_interval(MAX_RECONNECT_INTERVAL, CycleOutcome::Cancelled),
            Duration::ZERO
        );
    }

    #[derive(Clone, Default)]
    struct Stub {
        hello_sent: Arc<std::sync::atomic::AtomicBool>,
        heartbeats_received: Arc<AtomicU32>,
        last_pending_spool_bytes: Arc<AtomicU64>,
        /// When set, the server closes the socket after this many heartbeats
        /// instead of continuing to ack them -- used to exercise reconnect.
        close_after: Arc<AtomicU32>,
    }

    async fn management_socket(state: State<Stub>, ws: WebSocketUpgrade) -> impl IntoResponse {
        ws.on_upgrade(move |socket| serve_management(socket, state.0))
    }

    async fn serve_management(mut socket: WebSocket, stub: Stub) {
        let hello = Hello {
            protocol_version: PROTOCOL_VERSION,
            agent_version: "cloud-test-backend".into(),
            capabilities: vec![Capability::TelemetryShipping],
        };
        let envelope = Envelope::new("hello", &hello).expect("server hello must encode");
        if socket
            .send(AxumMessage::Text(
                serde_json::to_string(&envelope).unwrap().into(),
            ))
            .await
            .is_err()
        {
            return;
        }
        stub.hello_sent.store(true, Ordering::SeqCst);

        while let Some(Ok(message)) = socket.recv().await {
            let AxumMessage::Text(text) = message else {
                continue;
            };
            let Ok(envelope) = serde_json::from_str::<Envelope>(&text) else {
                continue;
            };
            if let Some(heartbeat) = envelope.decode::<Heartbeat>("heartbeat") {
                stub.last_pending_spool_bytes
                    .store(heartbeat.pending_spool_bytes, Ordering::SeqCst);
                let count = stub.heartbeats_received.fetch_add(1, Ordering::SeqCst) + 1;
                let close_after = stub.close_after.load(Ordering::SeqCst);
                if close_after > 0 && count >= close_after {
                    let _ = socket.close().await;
                    return;
                }
                let ack = HeartbeatAck {
                    received_at_millis: 42,
                };
                let ack_envelope = Envelope::new("heartbeat_ack", &ack).expect("ack must encode");
                if socket
                    .send(AxumMessage::Text(
                        serde_json::to_string(&ack_envelope).unwrap().into(),
                    ))
                    .await
                    .is_err()
                {
                    return;
                }
            }
        }
    }

    /// Binds a real loopback listener, or returns `None` when the sandbox
    /// denies it -- the same graceful skip `flusher`'s tests use, so this
    /// suite behaves the same way in the same environments.
    async fn serve(stub: Stub) -> Option<String> {
        let app = Router::new()
            .route(
                "/v1/enroll",
                axum::routing::post(|| async {
                    axum::Json(serde_json::json!({
                        "tenant_id": Uuid::new_v4(),
                        "instance_token": "inst_heartbeat_test"
                    }))
                }),
            )
            .route("/v1/management", get(management_socket))
            .with_state(stub);
        let listener = match tokio::net::TcpListener::bind::<SocketAddr>(
            "127.0.0.1:0".parse().expect("loopback address must parse"),
        )
        .await
        {
            Ok(listener) => listener,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                eprintln!("skipping heartbeat network test: sandbox denied TCP bind");
                return None;
            }
            Err(error) => panic!("test server must bind: {error}"),
        };
        let address = listener.local_addr().expect("test server has an address");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Some(format!("http://{address}"))
    }

    async fn linked_test_link(backend_url: &str) -> (Arc<CloudLink>, tempfile::TempDir) {
        let directory = tempfile::tempdir().expect("temporary directory must be created");
        let link = Arc::new(CloudLink::load_for_loopback_development(
            directory.path().to_path_buf(),
            "heartbeat-test",
        ));
        link.configure(
            BackendUrl::loopback_development(backend_url)
                .expect("stub backend URL must be accepted"),
        )
        .expect("test link must be configured");
        link.enroll("heartbeat-test-code")
            .await
            .expect("test link must enroll");
        (link, directory)
    }

    #[tokio::test]
    async fn the_hello_handshake_negotiates_before_any_heartbeat_is_sent() {
        let stub = Stub::default();
        let Some(backend_url) = serve(stub.clone()).await else {
            return;
        };
        let (link, _directory) = linked_test_link(&backend_url).await;

        let (cancel_tx, cancel_rx) = watch::channel(false);
        let mut cancel = cancel_rx;
        let link_for_cycle = link.clone();
        let cycle =
            tokio::spawn(async move { connection_cycle(&link_for_cycle, &mut cancel).await });

        tokio::time::timeout(Duration::from_secs(5), async {
            while !stub.hello_sent.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("server hello must be observed quickly");

        cancel_tx.send(true).expect("send shutdown signal");
        let outcome = tokio::time::timeout(Duration::from_secs(5), cycle)
            .await
            .expect("connection cycle must stop promptly on cancellation")
            .expect("connection cycle must not panic");
        assert_eq!(outcome, CycleOutcome::Cancelled);
    }

    #[tokio::test]
    async fn a_heartbeat_is_sent_and_acknowledged_carrying_live_spool_depth() {
        let stub = Stub::default();
        let Some(backend_url) = serve(stub.clone()).await else {
            return;
        };
        let (link, _directory) = linked_test_link(&backend_url).await;
        link.set_feature_switches(crate::CloudFeatureSwitches {
            telemetry: true,
            ..Default::default()
        })
        .expect("enable telemetry export");
        link.record(vec![temps_cloud_protocol::SpanRecord {
            trace_id: "heartbeat-trace".into(),
            span_id: "heartbeat-span".into(),
            name: "heartbeat".into(),
            ts_millis: 1,
            duration_ms: 1.0,
            attributes: Default::default(),
            ..Default::default()
        }]);
        assert!(
            link.spooled_bytes() > 0,
            "the fixture must have queued something to report"
        );

        let (cancel_tx, cancel_rx) = watch::channel(false);
        let mut cancel = cancel_rx;
        let link_for_cycle = link.clone();
        let cycle =
            tokio::spawn(async move { connection_cycle(&link_for_cycle, &mut cancel).await });

        tokio::time::timeout(Duration::from_secs(5), async {
            while stub.heartbeats_received.load(Ordering::SeqCst) == 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("at least one heartbeat must be observed");

        assert!(stub.last_pending_spool_bytes.load(Ordering::SeqCst) > 0);

        cancel_tx.send(true).expect("send shutdown signal");
        tokio::time::timeout(Duration::from_secs(5), cycle)
            .await
            .expect("connection cycle must stop promptly on cancellation")
            .expect("connection cycle must not panic");
    }

    #[tokio::test]
    async fn a_server_close_is_reported_as_disconnected_so_the_caller_reconnects() {
        let stub = Stub {
            close_after: Arc::new(AtomicU32::new(1)),
            ..Default::default()
        };
        let Some(backend_url) = serve(stub.clone()).await else {
            return;
        };
        let (link, _directory) = linked_test_link(&backend_url).await;

        let (_cancel_tx, cancel_rx) = watch::channel(false);
        let mut cancel = cancel_rx;
        let outcome =
            tokio::time::timeout(Duration::from_secs(5), connection_cycle(&link, &mut cancel))
                .await
                .expect("the cycle must end once the server closes the socket");

        assert_eq!(
            outcome,
            CycleOutcome::Disconnected,
            "a server-initiated close must be classified so the caller schedules a reconnect, \
             never treated the same as an explicit shutdown"
        );
    }

    #[tokio::test]
    async fn an_unlinked_instance_never_attempts_a_connection() {
        let directory = tempfile::tempdir().expect("temporary directory must be created");
        let link = Arc::new(CloudLink::load_for_loopback_development(
            directory.path().to_path_buf(),
            "heartbeat-test",
        ));
        // Never configured or enrolled.
        let (_cancel_tx, cancel_rx) = watch::channel(false);
        let mut cancel = cancel_rx;
        let outcome = connection_cycle(&link, &mut cancel).await;
        assert_eq!(outcome, CycleOutcome::NotLinked);
    }
}
