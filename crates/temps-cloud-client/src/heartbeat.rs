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
//!
//! # Instance status (ADR-039)
//!
//! When both sides negotiate `Capability::InstanceStatusReporting`, this same
//! connection also carries a [`StatusReport`] — see [`STATUS_REPORT_INTERVAL`]
//! for why that runs on its own, much slower cadence than the heartbeat
//! above. Reporting is entirely opt-in and silent when unavailable: an old
//! server that never negotiates the capability, or a `status_provider` of
//! `None` because the host layer has not wired one up yet, both simply mean
//! no `StatusReport` is ever built or sent — never a panic, never a logged
//! error the operator has to make sense of.

use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::{stream::SplitSink, stream::SplitStream, SinkExt, StreamExt};
use tokio::sync::watch;
use tokio_tungstenite::{
    tungstenite::{client::IntoClientRequest, http::header::AUTHORIZATION, Message},
    MaybeTlsStream, WebSocketStream,
};
use uuid::Uuid;

use temps_cloud_protocol::{
    truncate_status_text, Capability, Envelope, Heartbeat, HeartbeatAck, Hello, StatusReport,
    StatusRequest, StatusSelfUpdate, PROTOCOL_VERSION,
};

use crate::link::CloudLink;
use crate::status_provider::StatusProvider;
use crate::BackendUrl;

type WsStream = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;
type WsWrite = SplitSink<WsStream, Message>;
type WsRead = SplitStream<WsStream>;

/// Cadence for a healthy connection. Comfortably under the server's
/// heartbeat-idle timeout (90s), matching the interval called out in the
/// protocol design as the common, safe choice.
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);

/// Cadence for the separate, slower [`StatusReport`] once
/// `Capability::InstanceStatusReporting` is negotiated.
///
/// Deliberately not [`HEARTBEAT_INTERVAL`]: a status report carries
/// slow-moving facts (binary version, deployment/service/project counts,
/// self-update state) that legitimately change on the order of minutes, not
/// seconds, and building one costs a [`StatusProvider::snapshot`] call the
/// host layer will typically implement with a database read. Running that on
/// the heartbeat's 30s hot path would put a DB query behind the one signal
/// this whole module exists to keep cheap and reliable, for a console value
/// that would just show the same number six times a minute. Five minutes
/// matches the slow end of this crate's own polling cadences
/// ([`crate::flusher::MAX_INTERVAL`]) and keeps a self-hosted operator's Cloud
/// console within a few minutes of "current" at negligible cost — roughly one
/// lightweight counting query every five minutes, immaterial next to what
/// serving a single deployment or request already costs on the reference 3
/// vCPU / 4 GB hardware. A [`StatusRequest`] nudge from Cloud can still get a
/// fresher read sooner (see [`heartbeat_loop`]); this interval only bounds
/// the *unprompted* cadence.
pub const STATUS_REPORT_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// Bound on a single [`StatusProvider::snapshot`] call.
///
/// This loop shares one `tokio::select!` between heartbeats, socket reads,
/// cancellation, and status reports: while one arm's future is running, none
/// of the others are polled. An unbounded `snapshot()` call (a real
/// implementation will typically be a database read) could therefore stall
/// heartbeats past the server's idle window and delay clean shutdown for as
/// long as the call hangs — this is not something every future
/// `StatusProvider` implementation can be trusted to bound on its own, so it
/// is enforced at the one call site instead. Five seconds matches this
/// codebase's own convention for a single bounded database operation (see
/// `CLAUDE.md`'s timeout-handling example) and sits comfortably under both
/// [`HEARTBEAT_INTERVAL`] (30s) and the server's heartbeat-idle timeout
/// (90s), so a single slow snapshot can cost at most one skipped status
/// report, never a missed heartbeat or a wedged shutdown.
const STATUS_SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(5);

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
///
/// `status_provider` supplies the data for [`StatusReport`] (ADR-039) that
/// only the host layer can compute — see [`crate::status_provider`]. Passing
/// `None` is a normal, silent state: the capability is still negotiated with
/// the server on every connection, but no report is ever built or sent until
/// a provider is registered. This lets a build wire up this task before the
/// host layer has a `StatusProvider` implementation ready, with no behavior
/// change beyond the capability appearing in `Hello`.
pub async fn run(
    link: Arc<CloudLink>,
    mut cancel: watch::Receiver<bool>,
    status_provider: Option<Arc<dyn StatusProvider>>,
) {
    tracing::info!("Cloud heartbeat sender started");
    // Approximates process uptime: this task is spawned once, unconditionally,
    // at instance startup (see the doc comment above), so capturing the clock
    // here is close enough without needing a second, separately-threaded
    // process-start timestamp just for this one field.
    let started_at = Instant::now();
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
                let outcome = connection_cycle(&link, &mut cancel, status_provider.as_ref(), started_at).await;
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
    status_provider: Option<&Arc<dyn StatusProvider>>,
    started_at: Instant,
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

    let negotiated = match handshake(link, &mut write, &mut read).await {
        Ok(negotiated) => negotiated,
        Err(error) => {
            tracing::warn!(%error, "Cloud heartbeat handshake failed; will retry");
            let _ = write.close().await;
            return CycleOutcome::Disconnected;
        }
    };
    tracing::info!(
        instance_status_reporting = negotiated.contains(&Capability::InstanceStatusReporting),
        "Cloud heartbeat connection established and negotiated"
    );

    heartbeat_loop(
        link,
        instance_id,
        &negotiated,
        status_provider,
        started_at,
        &mut write,
        &mut read,
        cancel,
    )
    .await
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
/// [`HANDSHAKE_TIMEOUT`], then return the capabilities both sides advertised
/// so the caller knows what it may actually use on this connection.
async fn handshake(
    link: &CloudLink,
    write: &mut WsWrite,
    read: &mut WsRead,
) -> Result<Vec<Capability>, HeartbeatError> {
    let server_hello = tokio::time::timeout(HANDSHAKE_TIMEOUT, read.next())
        .await
        .map_err(|_| HeartbeatError::HelloTimeout)?
        .ok_or(HeartbeatError::ConnectionClosedDuringHandshake)?
        .map_err(|source| HeartbeatError::Connect { source })
        .and_then(|message| decode_hello(&message))?;

    let our_capabilities = vec![
        Capability::TelemetryShipping,
        Capability::ManagedAiInference,
        Capability::InstanceStatusReporting,
    ];
    let hello = Hello {
        protocol_version: PROTOCOL_VERSION,
        agent_version: link.agent_version().to_string(),
        capabilities: our_capabilities.clone(),
    };
    send_envelope(write, "hello", &hello).await?;

    Ok(negotiated_capabilities(
        &our_capabilities,
        &server_hello.capabilities,
    ))
}

/// The capabilities we offered that the server also advertised — what this
/// connection may actually use. Separated out from [`handshake`] so the
/// intersection logic is testable without a real socket, matching how
/// [`next_reconnect_interval`] keeps this module's other per-connection
/// policy decisions pure and fast to test.
fn negotiated_capabilities(ours: &[Capability], theirs: &[Capability]) -> Vec<Capability> {
    ours.iter()
        .copied()
        .filter(|capability| theirs.contains(capability))
        .collect()
}

/// Whether a [`StatusReport`] may be sent on this connection at all.
///
/// Both conditions are required: the server must have advertised
/// `Capability::InstanceStatusReporting` back to us (never assume — the
/// crate's own hard rule), and the host layer must actually have registered a
/// [`StatusProvider`], or there is nothing honest to put in the report.
/// Pulled out as a pure function so the negotiation gate is testable without
/// a socket, the same reasoning as [`negotiated_capabilities`].
fn status_reporting_enabled(negotiated: &[Capability], has_status_provider: bool) -> bool {
    has_status_provider && negotiated.contains(&Capability::InstanceStatusReporting)
}

/// Enforce [`temps_cloud_protocol::MAX_STATUS_TEXT_CHARS`] on the free-text
/// fields of a self-update snapshot at the point it is copied into an
/// outgoing [`StatusReport`].
///
/// [`StatusSelfUpdate`]'s own doc comment is explicit that the wire type does
/// not re-validate this itself — some producer on the send path has to, and
/// that must not depend on every [`StatusProvider`] implementation
/// remembering to call [`truncate_status_text`] before returning its
/// snapshot. Enforcing it here, once, on the way to the wire, makes the bound
/// hold regardless of what any given provider does.
fn cap_self_update_text(mut self_update: StatusSelfUpdate) -> StatusSelfUpdate {
    self_update.blocker_reason = self_update
        .blocker_reason
        .as_deref()
        .map(truncate_status_text);
    if let Some(attempt) = self_update.last_attempt.as_mut() {
        attempt.error = attempt.error.as_deref().map(truncate_status_text);
    }
    self_update
}

/// Build and send one [`StatusReport`], filling in the parts this crate knows
/// about itself (`instance_id`, `temps_version`, `uptime_seconds`) around the
/// snapshot from `status_provider`.
///
/// The snapshot call is bounded by [`STATUS_SNAPSHOT_TIMEOUT`]: a timeout is
/// logged and treated as "nothing to report this cycle" (`Ok(())`), never as
/// a connection failure — the whole point of the bound is that a slow
/// provider must not be able to take the management connection down with it.
async fn send_status_report(
    write: &mut WsWrite,
    link: &CloudLink,
    instance_id: Uuid,
    started_at: Instant,
    status_provider: &Arc<dyn StatusProvider>,
) -> Result<(), HeartbeatError> {
    let snapshot =
        match tokio::time::timeout(STATUS_SNAPSHOT_TIMEOUT, status_provider.snapshot()).await {
            Ok(snapshot) => snapshot,
            Err(_) => {
                tracing::warn!(
                    timeout_secs = STATUS_SNAPSHOT_TIMEOUT.as_secs(),
                    "Cloud status provider snapshot timed out; skipping this status report cycle"
                );
                return Ok(());
            }
        };
    let report = StatusReport {
        instance_id,
        temps_version: link.agent_version().to_string(),
        uptime_seconds: started_at.elapsed().as_secs(),
        deployment_count: snapshot.deployment_count,
        service_count: snapshot.service_count,
        project_count: snapshot.project_count,
        resources: snapshot.resources,
        self_update: snapshot.self_update.map(cap_self_update_text),
    };
    send_envelope(write, "status_report", &report).await?;
    tracing::debug!("Cloud status report sent");
    Ok(())
}

/// Send heartbeats on [`HEARTBEAT_INTERVAL`] and read back acknowledgements
/// until the connection ends or shutdown is requested. Also sends a
/// [`StatusReport`] on [`STATUS_REPORT_INTERVAL`] once negotiated, and again
/// immediately whenever the server sends a [`StatusRequest`] nudge — see the
/// module docs.
#[allow(clippy::too_many_arguments)]
async fn heartbeat_loop(
    link: &CloudLink,
    instance_id: Uuid,
    negotiated: &[Capability],
    status_provider: Option<&Arc<dyn StatusProvider>>,
    started_at: Instant,
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

    let status_reporting_enabled = status_reporting_enabled(negotiated, status_provider.is_some());
    // Created unconditionally to keep the `select!` arm below unconditional
    // too -- when reporting is disabled the tick is simply skipped every
    // time, which costs nothing. Its first tick also fires immediately, which
    // is what gives a freshly (re)negotiated connection its "report once on
    // connect" behavior for free, matching the heartbeat ticker above.
    let mut status_ticker = tokio::time::interval(STATUS_REPORT_INTERVAL);
    status_ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

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
            _ = status_ticker.tick(), if status_reporting_enabled => {
                // `status_provider` is `Some` whenever this branch can run:
                // `status_reporting_enabled` already required it above.
                if let Some(status_provider) = status_provider {
                    if let Err(error) =
                        send_status_report(write, link, instance_id, started_at, status_provider).await
                    {
                        tracing::debug!(%error, "Cloud status report send failed; will reconnect");
                        return CycleOutcome::Disconnected;
                    }
                }
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
                        } else if decode_status_request(&message).is_some() {
                            // A request, not a command (see `StatusRequest`'s
                            // own doc comment): we honor it on a best-effort
                            // basis when we have something to answer with,
                            // and silently do nothing otherwise -- there is no
                            // obligation to reply, and no error to report.
                            if status_reporting_enabled {
                                if let Some(status_provider) = status_provider {
                                    tracing::debug!("Cloud requested a status report refresh");
                                    if let Err(error) = send_status_report(
                                        write, link, instance_id, started_at, status_provider,
                                    )
                                    .await
                                    {
                                        tracing::debug!(
                                            %error,
                                            "Cloud status report send failed; will reconnect"
                                        );
                                        return CycleOutcome::Disconnected;
                                    }
                                }
                            }
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

fn decode_status_request(message: &Message) -> Option<StatusRequest> {
    decode_envelope(message)?.decode::<StatusRequest>("status_request")
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
    use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
    use std::sync::Mutex;

    use async_trait::async_trait;
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
    use crate::status_provider::StatusSnapshot;

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
        /// Additional capabilities this stub advertises in its own `Hello`,
        /// beyond the `TelemetryShipping` every test in this file already
        /// relies on. Empty by default so existing tests are unaffected;
        /// status-reporting tests add `Capability::InstanceStatusReporting`.
        extra_server_capabilities: Arc<Mutex<Vec<Capability>>>,
        status_reports_received: Arc<AtomicU32>,
        last_status_report: Arc<Mutex<Option<StatusReport>>>,
        /// When true, the server replies to the *first* status report it
        /// receives with a `StatusRequest` nudge, so a test can observe the
        /// "report again on request" path without waiting out
        /// `STATUS_REPORT_INTERVAL`.
        nudge_after_first_status_report: Arc<AtomicBool>,
    }

    async fn management_socket(state: State<Stub>, ws: WebSocketUpgrade) -> impl IntoResponse {
        ws.on_upgrade(move |socket| serve_management(socket, state.0))
    }

    async fn serve_management(mut socket: WebSocket, stub: Stub) {
        let mut capabilities = vec![Capability::TelemetryShipping];
        capabilities.extend(
            stub.extra_server_capabilities
                .lock()
                .expect("extra server capabilities lock")
                .iter()
                .copied(),
        );
        let hello = Hello {
            protocol_version: PROTOCOL_VERSION,
            agent_version: "cloud-test-backend".into(),
            capabilities,
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
            } else if let Some(report) = envelope.decode::<StatusReport>("status_report") {
                let count = stub.status_reports_received.fetch_add(1, Ordering::SeqCst) + 1;
                *stub
                    .last_status_report
                    .lock()
                    .expect("last status report lock") = Some(report);
                if count == 1 && stub.nudge_after_first_status_report.load(Ordering::SeqCst) {
                    let request = StatusRequest { instance_id: None };
                    let request_envelope =
                        Envelope::new("status_request", &request).expect("request must encode");
                    if socket
                        .send(AxumMessage::Text(
                            serde_json::to_string(&request_envelope).unwrap().into(),
                        ))
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
            }
        }
    }

    /// A [`StatusProvider`] returning a fixed [`StatusSnapshot`], for tests
    /// that need to observe exactly what an instance sends without any real
    /// database or system access.
    struct FixedStatusProvider(StatusSnapshot);

    #[async_trait]
    impl StatusProvider for FixedStatusProvider {
        async fn snapshot(&self) -> StatusSnapshot {
            self.0.clone()
        }
    }

    /// A [`StatusProvider`] whose `snapshot()` never resolves inside the test
    /// window, for exercising [`STATUS_SNAPSHOT_TIMEOUT`].
    struct HangingStatusProvider;

    #[async_trait]
    impl StatusProvider for HangingStatusProvider {
        async fn snapshot(&self) -> StatusSnapshot {
            std::future::pending().await
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
        let cycle = tokio::spawn(async move {
            connection_cycle(&link_for_cycle, &mut cancel, None, Instant::now()).await
        });

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
        let cycle = tokio::spawn(async move {
            connection_cycle(&link_for_cycle, &mut cancel, None, Instant::now()).await
        });

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
        let outcome = tokio::time::timeout(
            Duration::from_secs(5),
            connection_cycle(&link, &mut cancel, None, Instant::now()),
        )
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
        let outcome = connection_cycle(&link, &mut cancel, None, Instant::now()).await;
        assert_eq!(outcome, CycleOutcome::NotLinked);
    }

    #[test]
    fn negotiated_capabilities_is_the_intersection_of_both_sides() {
        let ours = [
            Capability::TelemetryShipping,
            Capability::InstanceStatusReporting,
        ];
        assert_eq!(
            negotiated_capabilities(&ours, &[Capability::TelemetryShipping]),
            vec![Capability::TelemetryShipping],
            "a capability the server never advertised must not be usable"
        );
        assert_eq!(
            negotiated_capabilities(
                &ours,
                &[
                    Capability::TelemetryShipping,
                    Capability::InstanceStatusReporting
                ]
            ),
            vec![
                Capability::TelemetryShipping,
                Capability::InstanceStatusReporting
            ]
        );
        assert!(negotiated_capabilities(&ours, &[]).is_empty());
    }

    #[test]
    fn status_reporting_requires_both_negotiation_and_a_registered_provider() {
        let negotiated = [Capability::InstanceStatusReporting];
        assert!(
            status_reporting_enabled(&negotiated, true),
            "both conditions are met"
        );
        assert!(
            !status_reporting_enabled(&negotiated, false),
            "negotiated but no provider registered: nothing honest to send"
        );
        assert!(
            !status_reporting_enabled(&[Capability::TelemetryShipping], true),
            "a provider without the server ever advertising the capability must not report"
        );
        assert!(!status_reporting_enabled(&[], false));
    }

    fn fixed_snapshot() -> StatusSnapshot {
        StatusSnapshot {
            deployment_count: 3,
            service_count: 5,
            project_count: 2,
            resources: None,
            self_update: None,
        }
    }

    /// The negotiation gate end to end: a server that never advertises
    /// `InstanceStatusReporting`, even though this instance has a real
    /// provider wired up and ready to report, must never receive a
    /// `status_report` frame. This is the crate's core "negotiate, never
    /// assume" rule -- verified on the wire, not just at the pure-function
    /// level above.
    #[tokio::test]
    async fn a_status_report_is_never_sent_when_the_server_does_not_negotiate_the_capability() {
        let stub = Stub::default(); // advertises only TelemetryShipping
        let Some(backend_url) = serve(stub.clone()).await else {
            return;
        };
        let (link, _directory) = linked_test_link(&backend_url).await;
        let provider: Arc<dyn StatusProvider> = Arc::new(FixedStatusProvider(fixed_snapshot()));

        let (cancel_tx, cancel_rx) = watch::channel(false);
        let mut cancel = cancel_rx;
        let link_for_cycle = link.clone();
        let cycle = tokio::spawn(async move {
            connection_cycle(
                &link_for_cycle,
                &mut cancel,
                Some(&provider),
                Instant::now(),
            )
            .await
        });

        tokio::time::timeout(Duration::from_secs(5), async {
            while !stub.hello_sent.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("server hello must be observed quickly");
        // Give the connection a real window in which an incorrectly-gated
        // "send immediately on connect" would have already fired.
        tokio::time::sleep(Duration::from_millis(200)).await;

        assert_eq!(
            stub.status_reports_received.load(Ordering::SeqCst),
            0,
            "a capability the server never advertised must never be used"
        );

        cancel_tx.send(true).expect("send shutdown signal");
        tokio::time::timeout(Duration::from_secs(5), cycle)
            .await
            .expect("connection cycle must stop promptly on cancellation")
            .expect("connection cycle must not panic");
    }

    /// A server with no `StatusProvider` registered at all (the "not wired up
    /// yet" state every host starts in) must be just as silent as an old
    /// server, even when the far side happily negotiates the capability.
    #[tokio::test]
    async fn a_status_report_is_never_sent_without_a_registered_provider() {
        let stub = Stub {
            extra_server_capabilities: Arc::new(Mutex::new(vec![
                Capability::InstanceStatusReporting,
            ])),
            ..Default::default()
        };
        let Some(backend_url) = serve(stub.clone()).await else {
            return;
        };
        let (link, _directory) = linked_test_link(&backend_url).await;

        let (cancel_tx, cancel_rx) = watch::channel(false);
        let mut cancel = cancel_rx;
        let link_for_cycle = link.clone();
        let cycle = tokio::spawn(async move {
            connection_cycle(&link_for_cycle, &mut cancel, None, Instant::now()).await
        });

        tokio::time::timeout(Duration::from_secs(5), async {
            while !stub.hello_sent.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("server hello must be observed quickly");
        tokio::time::sleep(Duration::from_millis(200)).await;

        assert_eq!(stub.status_reports_received.load(Ordering::SeqCst), 0);

        cancel_tx.send(true).expect("send shutdown signal");
        tokio::time::timeout(Duration::from_secs(5), cycle)
            .await
            .expect("connection cycle must stop promptly on cancellation")
            .expect("connection cycle must not panic");
    }

    /// Once both sides negotiate the capability and a provider is
    /// registered, the very first status report arrives promptly (it does
    /// not wait out `STATUS_REPORT_INTERVAL`) and carries the provider's
    /// counts alongside this crate's own version/uptime/instance id.
    #[tokio::test]
    async fn a_status_report_is_sent_immediately_once_negotiated_with_a_provider() {
        let stub = Stub {
            extra_server_capabilities: Arc::new(Mutex::new(vec![
                Capability::InstanceStatusReporting,
            ])),
            ..Default::default()
        };
        let Some(backend_url) = serve(stub.clone()).await else {
            return;
        };
        let (link, _directory) = linked_test_link(&backend_url).await;
        let instance_id = link
            .instance_id()
            .expect("test link must have an instance id");
        let provider: Arc<dyn StatusProvider> = Arc::new(FixedStatusProvider(fixed_snapshot()));

        let (cancel_tx, cancel_rx) = watch::channel(false);
        let mut cancel = cancel_rx;
        let link_for_cycle = link.clone();
        let cycle = tokio::spawn(async move {
            connection_cycle(
                &link_for_cycle,
                &mut cancel,
                Some(&provider),
                Instant::now(),
            )
            .await
        });

        tokio::time::timeout(Duration::from_secs(5), async {
            while stub.status_reports_received.load(Ordering::SeqCst) == 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("a status report must arrive promptly, not after STATUS_REPORT_INTERVAL");

        let report = stub
            .last_status_report
            .lock()
            .expect("last status report lock")
            .clone()
            .expect("a status report must have been recorded");
        assert_eq!(report.instance_id, instance_id);
        assert_eq!(report.temps_version, link.agent_version());
        assert_eq!(report.deployment_count, 3);
        assert_eq!(report.service_count, 5);
        assert_eq!(report.project_count, 2);
        assert!(report.resources.is_none());
        assert!(report.self_update.is_none());

        cancel_tx.send(true).expect("send shutdown signal");
        tokio::time::timeout(Duration::from_secs(5), cycle)
            .await
            .expect("connection cycle must stop promptly on cancellation")
            .expect("connection cycle must not panic");
    }

    /// The "please report sooner" nudge: after the automatic first report,
    /// the server asks for another one and must receive it promptly rather
    /// than waiting out the remainder of `STATUS_REPORT_INTERVAL`.
    #[tokio::test]
    async fn a_status_request_from_the_server_triggers_an_immediate_extra_report() {
        let stub = Stub {
            extra_server_capabilities: Arc::new(Mutex::new(vec![
                Capability::InstanceStatusReporting,
            ])),
            nudge_after_first_status_report: Arc::new(AtomicBool::new(true)),
            ..Default::default()
        };
        let Some(backend_url) = serve(stub.clone()).await else {
            return;
        };
        let (link, _directory) = linked_test_link(&backend_url).await;
        let provider: Arc<dyn StatusProvider> = Arc::new(FixedStatusProvider(fixed_snapshot()));

        let (cancel_tx, cancel_rx) = watch::channel(false);
        let mut cancel = cancel_rx;
        let link_for_cycle = link.clone();
        let cycle = tokio::spawn(async move {
            connection_cycle(
                &link_for_cycle,
                &mut cancel,
                Some(&provider),
                Instant::now(),
            )
            .await
        });

        tokio::time::timeout(Duration::from_secs(5), async {
            while stub.status_reports_received.load(Ordering::SeqCst) < 2 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect(
            "the nudge-triggered report must arrive promptly, not after STATUS_REPORT_INTERVAL",
        );

        cancel_tx.send(true).expect("send shutdown signal");
        tokio::time::timeout(Duration::from_secs(5), cycle)
            .await
            .expect("connection cycle must stop promptly on cancellation")
            .expect("connection cycle must not panic");
    }

    fn long_text() -> String {
        "x".repeat(temps_cloud_protocol::MAX_STATUS_TEXT_CHARS + 500)
    }

    fn self_update_with_free_text(
        blocker_reason: Option<String>,
        error: Option<String>,
    ) -> StatusSelfUpdate {
        StatusSelfUpdate {
            enabled: false,
            supervisor: temps_cloud_protocol::StatusSupervisorKind::None,
            restart_mode: temps_cloud_protocol::StatusSelfUpdateRestartMode::Manual,
            blocker: Some(temps_cloud_protocol::StatusSelfUpdateBlocker::BinaryNotWritable),
            blocker_reason,
            phase: temps_cloud_protocol::StatusSelfUpdatePhase::Failed,
            available_update: None,
            last_attempt: Some(temps_cloud_protocol::StatusSelfUpdateAttempt {
                status: temps_cloud_protocol::StatusSelfUpdateAttemptOutcome::Failed,
                from_version: "v0.2.9".into(),
                to_version: None,
                finished_at: None,
                error,
            }),
        }
    }

    #[test]
    fn cap_self_update_text_truncates_both_free_text_fields() {
        let long = long_text();
        let capped = cap_self_update_text(self_update_with_free_text(
            Some(long.clone()),
            Some(long.clone()),
        ));

        let blocker_reason = capped
            .blocker_reason
            .expect("blocker reason must remain set");
        assert!(blocker_reason.chars().count() < long.chars().count());
        assert!(blocker_reason.ends_with("…[truncated]"));

        let error = capped
            .last_attempt
            .expect("last attempt must remain set")
            .error
            .expect("error must remain set");
        assert!(error.chars().count() < long.chars().count());
        assert!(error.ends_with("…[truncated]"));
    }

    #[test]
    fn cap_self_update_text_leaves_short_text_and_absent_fields_untouched() {
        let self_update = self_update_with_free_text(None, None);
        let capped = cap_self_update_text(self_update.clone());
        assert_eq!(capped, self_update);

        let short = self_update_with_free_text(
            Some("binary not writable".into()),
            Some("permission denied".into()),
        );
        let capped = cap_self_update_text(short.clone());
        assert_eq!(capped, short, "short text must round-trip unchanged");
    }

    /// End to end: even a `StatusProvider` that forgets to bound its own
    /// free-text fields must never put an oversized frame on the wire -- the
    /// cap has to be enforced on the send path itself, not merely documented
    /// as the provider's responsibility.
    #[tokio::test]
    async fn oversized_self_update_text_is_truncated_before_it_reaches_the_wire() {
        let long = long_text();
        let stub = Stub {
            extra_server_capabilities: Arc::new(Mutex::new(vec![
                Capability::InstanceStatusReporting,
            ])),
            ..Default::default()
        };
        let Some(backend_url) = serve(stub.clone()).await else {
            return;
        };
        let (link, _directory) = linked_test_link(&backend_url).await;
        let snapshot = StatusSnapshot {
            self_update: Some(self_update_with_free_text(
                Some(long.clone()),
                Some(long.clone()),
            )),
            ..fixed_snapshot()
        };
        let provider: Arc<dyn StatusProvider> = Arc::new(FixedStatusProvider(snapshot));

        let (cancel_tx, cancel_rx) = watch::channel(false);
        let mut cancel = cancel_rx;
        let link_for_cycle = link.clone();
        let cycle = tokio::spawn(async move {
            connection_cycle(
                &link_for_cycle,
                &mut cancel,
                Some(&provider),
                Instant::now(),
            )
            .await
        });

        tokio::time::timeout(Duration::from_secs(5), async {
            while stub.status_reports_received.load(Ordering::SeqCst) == 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("a status report must arrive");

        let report = stub
            .last_status_report
            .lock()
            .expect("last status report lock")
            .clone()
            .expect("a status report must have been recorded");
        let self_update = report.self_update.expect("self update must be present");
        let blocker_reason = self_update
            .blocker_reason
            .expect("blocker reason must be present");
        let error = self_update
            .last_attempt
            .expect("last attempt must be present")
            .error
            .expect("error must be present");
        assert!(
            blocker_reason.chars().count() < long.chars().count(),
            "the oversized blocker reason must have been truncated before it left the instance"
        );
        assert!(blocker_reason.ends_with("…[truncated]"));
        assert!(
            error.chars().count() < long.chars().count(),
            "the oversized attempt error must have been truncated before it left the instance"
        );
        assert!(error.ends_with("…[truncated]"));

        cancel_tx.send(true).expect("send shutdown signal");
        tokio::time::timeout(Duration::from_secs(5), cycle)
            .await
            .expect("connection cycle must stop promptly on cancellation")
            .expect("connection cycle must not panic");
    }

    /// The bug this guards against: without a bound on `snapshot()`, a single
    /// stalled provider call would occupy the `select!` loop indefinitely,
    /// silently suppressing heartbeats and blocking cancellation for as long
    /// as it hangs. A provider that never resolves must instead cost at most
    /// one skipped status report, with heartbeats and shutdown unaffected.
    #[tokio::test]
    async fn a_hanging_status_provider_is_bounded_and_never_wedges_the_loop() {
        let stub = Stub {
            extra_server_capabilities: Arc::new(Mutex::new(vec![
                Capability::InstanceStatusReporting,
            ])),
            ..Default::default()
        };
        let Some(backend_url) = serve(stub.clone()).await else {
            return;
        };
        let (link, _directory) = linked_test_link(&backend_url).await;
        let provider: Arc<dyn StatusProvider> = Arc::new(HangingStatusProvider);

        let (cancel_tx, cancel_rx) = watch::channel(false);
        let mut cancel = cancel_rx;
        let link_for_cycle = link.clone();
        let cycle = tokio::spawn(async move {
            connection_cycle(
                &link_for_cycle,
                &mut cancel,
                Some(&provider),
                Instant::now(),
            )
            .await
        });

        // Let the immediate on-connect tick fire and time out on its own --
        // this is what proves the bound actually applies, rather than the
        // call coincidentally finishing fast.
        tokio::time::sleep(STATUS_SNAPSHOT_TIMEOUT + Duration::from_millis(500)).await;
        assert_eq!(
            stub.status_reports_received.load(Ordering::SeqCst),
            0,
            "a snapshot call that never resolves must never produce a report"
        );

        // The loop must still be responsive to cancellation: proof it was
        // never wedged on the hanging snapshot call.
        cancel_tx.send(true).expect("send shutdown signal");
        tokio::time::timeout(Duration::from_secs(5), cycle)
            .await
            .expect(
                "connection cycle must remain responsive to cancellation \
                 even after a snapshot timeout",
            )
            .expect("connection cycle must not panic");
    }
}
