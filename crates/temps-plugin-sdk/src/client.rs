// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Typed client for querying the Temps platform over the channel.
//!
//! [`TempsClient`] is available on [`PluginContext`] and provides
//! structured, read-only access to platform data (projects, environments,
//! deployments).  All calls go through the WebSocket channel that Temps
//! opens to the plugin — the plugin never touches the database directly.
//!
//! # Example
//!
//! ```rust,no_run
//! use temps_plugin_sdk::prelude::*;
//!
//! async fn example(ctx: &PluginContext) {
//!     let client = ctx.temps();
//!
//!     let project = client.get_project(42).await.unwrap();
//!     println!("Project: {}", project.name);
//!
//!     let envs = client.list_environments(42).await.unwrap();
//!     for env in envs {
//!         println!("  env: {} ({})", env.name, env.slug);
//!     }
//! }
//! ```

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use futures::stream::StreamExt;
use futures::SinkExt;
use temps_plugin_protocol::channel::*;
use tokio::sync::{mpsc, oneshot, RwLock};
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, error, warn};

use crate::error::PluginSdkError;

/// A typed, async client for querying the Temps platform.
///
/// Obtained via [`PluginContext::temps()`].  All methods are async and
/// return `Result<T, PluginSdkError>`.
#[derive(Clone)]
pub struct TempsClient {
    inner: Arc<TempsClientInner>,
}

struct TempsClientInner {
    /// Sender for outgoing channel messages (requests) on the current
    /// connection, or `None` while the platform is not connected. Every
    /// request has a caller awaiting its response, so the queue only holds
    /// what the plugin's own concurrency puts in flight; the bound adds
    /// back-pressure.
    connection: std::sync::RwLock<Option<Connection>>,
    /// Pending request waiters, keyed by request id, each with the
    /// connection it was sent on.
    pending: RwLock<HashMap<u64, (u64, oneshot::Sender<ChannelResponse>)>>,
    /// Monotonic request ID counter.
    next_id: AtomicU64,
    /// Counts connections, so a connection that ends only fails the requests
    /// that were sent on it.
    next_connection: AtomicU64,
    /// Where platform events go, whichever connection delivers them.
    events: EventSender,
}

/// The sending half of one platform connection.
#[derive(Clone)]
struct Connection {
    id: u64,
    tx: mpsc::Sender<ChannelMessage>,
}

impl TempsClientInner {
    /// A connection is gone: forget it unless a newer one already took its
    /// place, and fail only the requests that were sent on it.
    async fn connection_ended(&self, connection_id: u64) {
        {
            let mut current = match self.connection.write() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            if current.as_ref().is_some_and(|c| c.id == connection_id) {
                *current = None;
            }
        }
        let mut pending = self.pending.write().await;
        let lost: Vec<u64> = pending
            .iter()
            .filter(|(_, (connection, _))| *connection == connection_id)
            .map(|(id, _)| *id)
            .collect();
        for id in lost {
            if let Some((_, waiter)) = pending.remove(&id) {
                let _ = waiter.send(ChannelResponse::err(
                    id,
                    ChannelErrorCode::Internal,
                    "Channel closed",
                ));
            }
        }
    }
}

/// Requests queued for the platform before `call` waits for room.
const REQUEST_CHANNEL_CAPACITY: usize = 64;

/// Platform events buffered for the plugin's handler. When the handler falls
/// this far behind, further events are dropped (and logged) instead of
/// blocking the reader, which also delivers the responses the handler may be
/// waiting on.
const EVENT_CHANNEL_CAPACITY: usize = 256;

/// Sender half used by the runtime to forward events to the plugin.
pub type EventSender = mpsc::Sender<temps_plugin_protocol::PluginEvent>;
/// Receiver half consumed by the runtime to deliver events.
pub type EventReceiver = mpsc::Receiver<temps_plugin_protocol::PluginEvent>;

impl TempsClient {
    /// Create a client that is not connected to the platform yet.
    ///
    /// Calls fail with [`PluginSdkError::ChannelClosed`] until
    /// [`Self::attach`] is given a connection. Returns
    /// `(client, event_receiver)`; the receiver yields the platform events of
    /// every connection the client is attached to over its lifetime.
    pub(crate) fn detached() -> (Self, EventReceiver) {
        let (events, event_rx) = mpsc::channel(EVENT_CHANNEL_CAPACITY);
        let inner = Arc::new(TempsClientInner {
            connection: std::sync::RwLock::new(None),
            pending: RwLock::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            next_connection: AtomicU64::new(1),
            events,
        });
        (Self { inner }, event_rx)
    }

    /// Create a new client wired to a WebSocket connection.
    ///
    /// Returns `(client, event_receiver)`.  The event receiver yields
    /// platform events pushed over the channel.
    #[cfg(test)]
    pub(crate) fn from_ws<S>(ws_stream: S) -> (Self, EventReceiver)
    where
        S: futures::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>>
            + futures::Sink<Message, Error = tokio_tungstenite::tungstenite::Error>
            + Send
            + Unpin
            + 'static,
    {
        let (client, event_rx) = Self::detached();
        client.attach(ws_stream);
        (client, event_rx)
    }

    /// Whether the platform is connected right now.
    pub fn is_connected(&self) -> bool {
        self.connection().is_some()
    }

    fn connection(&self) -> Option<Connection> {
        match self.inner.connection.read() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    /// Use `ws_stream` as the platform connection from now on.
    ///
    /// This is called by the plugin runtime after accepting the platform's
    /// authenticated WebSocket on `/_temps/channel`. A plugin that runs as a
    /// service outlives the platform process, so the platform connects again
    /// after it restarts: the new connection replaces the previous one, and
    /// requests still waiting on the previous one fail with "Channel closed".
    ///
    /// It spawns background tasks that write requests and dispatch responses
    /// and events.
    pub(crate) fn attach<S>(&self, ws_stream: S)
    where
        S: futures::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>>
            + futures::Sink<Message, Error = tokio_tungstenite::tungstenite::Error>
            + Send
            + Unpin
            + 'static,
    {
        let (mut ws_tx, mut ws_rx) = ws_stream.split();

        // Outgoing message sender (requests from client methods)
        let (msg_tx, mut msg_rx) = mpsc::channel::<ChannelMessage>(REQUEST_CHANNEL_CAPACITY);
        let connection_id = self.inner.next_connection.fetch_add(1, Ordering::Relaxed);
        let connection = Connection {
            id: connection_id,
            tx: msg_tx,
        };
        // Replacing the stored sender drops the previous one, which ends the
        // previous connection's writer and closes its socket.
        match self.inner.connection.write() {
            Ok(mut guard) => *guard = Some(connection),
            Err(poisoned) => *poisoned.into_inner() = Some(connection),
        }

        // Writer task: forwards queued messages to the WebSocket
        let writer_inner = self.inner.clone();
        tokio::spawn(async move {
            while let Some(msg) = msg_rx.recv().await {
                let json = match serde_json::to_string(&msg) {
                    Ok(j) => j,
                    Err(e) => {
                        error!("Failed to serialize channel request: {}", e);
                        continue;
                    }
                };
                if let Err(e) = ws_tx.send(Message::Text(json.into())).await {
                    debug!("Channel WebSocket write error: {}", e);
                    break;
                }
            }
            let _ = ws_tx.close().await;
            // Stop accepting requests before failing the ones in flight, so
            // none can be queued behind a writer that no longer writes.
            drop(msg_rx);
            writer_inner.connection_ended(connection_id).await;
        });

        // Reader task: dispatches incoming responses and events
        let reader_inner = self.inner.clone();
        tokio::spawn(async move {
            while let Some(frame) = ws_rx.next().await {
                let text = match frame {
                    Ok(Message::Text(t)) => t,
                    Ok(Message::Close(_)) => {
                        debug!("Platform closed the channel");
                        break;
                    }
                    Ok(Message::Ping(_)) | Ok(Message::Pong(_)) => continue,
                    Ok(_) => continue,
                    Err(e) => {
                        debug!("Channel read error: {}", e);
                        break;
                    }
                };

                let msg: ChannelMessage = match serde_json::from_str(&text) {
                    Ok(m) => m,
                    Err(e) => {
                        warn!("Invalid channel message from platform: {}", e);
                        continue;
                    }
                };

                match msg {
                    ChannelMessage::Response(resp) => {
                        let mut pending = reader_inner.pending.write().await;
                        if let Some((_, waiter)) = pending.remove(&resp.id) {
                            let _ = waiter.send(resp);
                        } else {
                            warn!("Received response for unknown request id {}", resp.id);
                        }
                    }
                    ChannelMessage::Event(evt) => {
                        if let Err(mpsc::error::TrySendError::Full(_)) =
                            reader_inner.events.try_send(evt.event)
                        {
                            warn!(
                                "Plugin event handler is {} events behind; dropping an event",
                                EVENT_CHANNEL_CAPACITY
                            );
                        }
                    }
                    ChannelMessage::Request(_) => {
                        warn!("Unexpected Request message from platform (ignoring)");
                    }
                }
            }

            reader_inner.connection_ended(connection_id).await;
        });
    }

    // ── Low-level request/response ─────────────────────────────────────

    /// Make a typed call and get back exactly that call's output.
    ///
    /// The return type is inferred from `C`, so a call and its reply cannot
    /// drift apart: pairing them is the [`PlatformCall`] impl's job, and
    /// that impl is generated alongside both wire enums.
    pub async fn call<C: PlatformCall>(&self, call: C) -> Result<C::Output, PluginSdkError> {
        let connection = self.connection().ok_or(PluginSdkError::ChannelClosed)?;
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);

        let (tx, rx) = oneshot::channel();
        self.inner
            .pending
            .write()
            .await
            .insert(id, (connection.id, tx));

        let msg = ChannelMessage::Request(ChannelRequest {
            id,
            call: call.into_request(),
        });

        if connection.tx.send(msg).await.is_err() {
            self.inner.pending.write().await.remove(&id);
            return Err(PluginSdkError::ChannelClosed);
        }

        let resp = rx.await.map_err(|_| PluginSdkError::ChannelClosed)?;

        match resp.outcome {
            CallOutcome::Ok(payload) => {
                C::output_from(*payload).map_err(|e| PluginSdkError::ProtocolMismatch {
                    reason: e.to_string(),
                })
            }
            CallOutcome::Err(err) => Err(PluginSdkError::PlatformError {
                code: format!("{:?}", err.code),
                message: err.message,
            }),
        }
    }

    // ── Typed query methods ────────────────────────────────────────────
    //
    // Thin wrappers over `call`. They exist for discoverability — a plugin
    // author should find `list_environments` by typing `client.` — not
    // because they add behaviour.

    /// Get a project by ID.
    pub async fn get_project(&self, project_id: i32) -> Result<ProjectInfo, PluginSdkError> {
        self.call(GetProject { project_id }).await
    }

    /// List all (non-deleted) projects.
    pub async fn list_projects(&self) -> Result<Vec<ProjectInfo>, PluginSdkError> {
        self.call(ListProjects {}).await
    }

    /// Get an environment by ID.
    pub async fn get_environment(
        &self,
        environment_id: i32,
    ) -> Result<EnvironmentInfo, PluginSdkError> {
        self.call(GetEnvironment { environment_id }).await
    }

    /// List environments for a project.
    pub async fn list_environments(
        &self,
        project_id: i32,
    ) -> Result<Vec<EnvironmentInfo>, PluginSdkError> {
        self.call(ListEnvironments { project_id }).await
    }

    /// Get a deployment by ID.
    pub async fn get_deployment(
        &self,
        deployment_id: i32,
    ) -> Result<DeploymentInfo, PluginSdkError> {
        self.call(GetDeployment { deployment_id }).await
    }

    /// Get the most recent deployment for a project, optionally filtered
    /// by environment.
    pub async fn get_last_deployment(
        &self,
        project_id: i32,
        environment_id: Option<i32>,
    ) -> Result<DeploymentInfo, PluginSdkError> {
        self.call(GetLastDeployment {
            project_id,
            environment_id,
        })
        .await
    }

    /// List deployments for a project, optionally filtered by environment.
    pub async fn list_deployments(
        &self,
        project_id: i32,
        environment_id: Option<i32>,
        limit: Option<u64>,
    ) -> Result<Vec<DeploymentInfo>, PluginSdkError> {
        self.call(ListDeployments {
            project_id,
            environment_id,
            limit,
        })
        .await
    }

    /// Publish an outbox record. Retry the same event id until acknowledged.
    pub async fn publish_watch_event(
        &self,
        event: PublishWatchEvent,
    ) -> Result<WatchEventAck, PluginSdkError> {
        self.call(event).await
    }

    /// Discover this durable plugin actor's currently effective host grants.
    pub async fn host_capabilities(&self) -> Result<HostCapabilities, PluginSdkError> {
        self.call(GetHostCapabilities {}).await
    }

    /// Generate text through the host's governed AI service. Provider
    /// credentials never cross the plugin boundary.
    pub async fn generate_ai(
        &self,
        request: GenerateAi,
    ) -> Result<GenerateAiResult, PluginSdkError> {
        self.call(request).await
    }

    /// Call the platform's own HTTP API as the user who called this plugin.
    ///
    /// Prefer the typed helpers on [`crate::api`], which fill in the path
    /// and method for you; this is the escape hatch for an endpoint the SDK
    /// has no wrapper for yet.
    pub async fn api_call(&self, call: ApiCall) -> Result<ApiCallResult, PluginSdkError> {
        self.call(call).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_temps_client_is_clone() {
        // Verify TempsClient is Clone (needed for PluginContext)
        fn assert_clone<T: Clone>() {}
        assert_clone::<TempsClient>();
    }

    /// Captures formatted log lines so a test can assert on the warnings.
    #[derive(Clone, Default)]
    struct CapturedLogs(Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for CapturedLogs {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CapturedLogs {
        type Writer = Self;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    impl CapturedLogs {
        fn text(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
        }
    }

    /// The platform pushes more events than the plugin's handler buffers
    /// while the handler reads none. The overflow is dropped with a warning
    /// per event, the oldest buffered events are kept, and the reader keeps
    /// delivering the response a request is waiting on instead of blocking
    /// behind the full event buffer.
    #[tokio::test]
    async fn events_beyond_the_buffer_are_dropped_while_requests_go_through() {
        use tokio_tungstenite::tungstenite::protocol::Role;
        use tokio_tungstenite::WebSocketStream;

        // Spawned tasks run on this thread (current-thread runtime), so the
        // thread-local subscriber sees the reader's warnings.
        let logs = CapturedLogs::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(logs.clone())
            .with_ansi(false)
            .with_max_level(tracing::Level::WARN)
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);

        let (plugin_io, platform_io) = tokio::io::duplex(64 * 1024);
        let plugin_ws = WebSocketStream::from_raw_socket(plugin_io, Role::Client, None).await;
        let mut platform_ws =
            WebSocketStream::from_raw_socket(platform_io, Role::Server, None).await;
        let (client, mut events) = TempsClient::from_ws(plugin_ws);

        const DROPPED: usize = 44;
        const SENT: usize = 256 + DROPPED;
        let platform = tokio::spawn(async move {
            for n in 0..SENT {
                let event: temps_plugin_protocol::PluginEvent =
                    serde_json::from_value(serde_json::json!({
                        "id": n.to_string(),
                        "event_type": "deployment.succeeded",
                        "timestamp": "2026-01-01T00:00:00Z",
                        "project_id": 1,
                        "data": {},
                    }))
                    .unwrap();
                let frame =
                    serde_json::to_string(&ChannelMessage::Event(ChannelEvent { event })).unwrap();
                platform_ws.send(Message::Text(frame.into())).await.unwrap();
            }
            // Answer the plugin's request only after every event: the
            // response is read behind all of them on the same socket.
            loop {
                let Some(Ok(Message::Text(text))) = platform_ws.next().await else {
                    panic!("the plugin's request never arrived");
                };
                if let Ok(ChannelMessage::Request(request)) = serde_json::from_str(&text) {
                    let response = ChannelMessage::Response(ChannelResponse::ok(
                        request.id,
                        PlatformCallResponse::ListProjects(Vec::new()),
                    ));
                    let frame = serde_json::to_string(&response).unwrap();
                    platform_ws.send(Message::Text(frame.into())).await.unwrap();
                    return platform_ws;
                }
            }
        });

        let projects =
            tokio::time::timeout(std::time::Duration::from_secs(10), client.list_projects())
                .await
                .expect("the request is answered while the event buffer is full")
                .expect("the platform's response is delivered");
        assert!(projects.is_empty());
        let _platform_ws = platform.await.unwrap();

        let mut received = Vec::new();
        while let Ok(event) = events.try_recv() {
            received.push(event.id);
        }
        assert_eq!(received.len(), 256, "the buffer holds exactly 256 events");
        assert_eq!(received.first().map(String::as_str), Some("0"));
        assert_eq!(received.last().map(String::as_str), Some("255"));

        let warnings = logs
            .text()
            .matches("events behind; dropping an event")
            .count();
        assert_eq!(warnings, DROPPED, "one warning per dropped event");
    }
}
