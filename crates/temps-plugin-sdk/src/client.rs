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
use temps_core::external_plugin::channel::*;
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
    /// Sender for outgoing channel messages (requests). Every request has a
    /// caller awaiting its response, so the queue only holds what the
    /// plugin's own concurrency puts in flight; the bound adds back-pressure.
    tx: mpsc::Sender<ChannelMessage>,
    /// Pending request waiters, keyed by request id.
    pending: RwLock<HashMap<u64, oneshot::Sender<ChannelResponse>>>,
    /// Monotonic request ID counter.
    next_id: AtomicU64,
}

/// Requests queued for the platform before `call` waits for room.
const REQUEST_CHANNEL_CAPACITY: usize = 64;

/// Platform events buffered for the plugin's handler. When the handler falls
/// this far behind, further events are dropped (and logged) instead of
/// blocking the reader, which also delivers the responses the handler may be
/// waiting on.
const EVENT_CHANNEL_CAPACITY: usize = 256;

/// Sender half used by the runtime to forward events to the plugin.
pub type EventSender = mpsc::Sender<temps_core::external_plugin::PluginEvent>;
/// Receiver half consumed by the runtime to deliver events.
pub type EventReceiver = mpsc::Receiver<temps_core::external_plugin::PluginEvent>;

impl TempsClient {
    /// Create a new client wired to a WebSocket connection.
    ///
    /// This is called by the plugin runtime after accepting the platform's
    /// incoming WebSocket on `/_temps/channel`.  It spawns a background task
    /// that reads responses/events and dispatches them.
    ///
    /// Returns `(client, event_receiver)`.  The event receiver yields
    /// platform events pushed over the channel.
    pub(crate) fn from_ws<S>(ws_stream: S) -> (Self, EventReceiver)
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

        let inner = Arc::new(TempsClientInner {
            tx: msg_tx.clone(),
            pending: RwLock::new(HashMap::new()),
            next_id: AtomicU64::new(1),
        });

        // Event channel — events received from the platform are forwarded here
        let (event_tx, event_rx) = mpsc::channel(EVENT_CHANNEL_CAPACITY);

        // Writer task: forwards queued messages to the WebSocket
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
        });

        // Reader task: dispatches incoming responses and events
        let reader_inner = inner.clone();
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
                        if let Some(waiter) = pending.remove(&resp.id) {
                            let _ = waiter.send(resp);
                        } else {
                            warn!("Received response for unknown request id {}", resp.id);
                        }
                    }
                    ChannelMessage::Event(evt) => {
                        if let Err(mpsc::error::TrySendError::Full(_)) =
                            event_tx.try_send(evt.event)
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

            // Channel closed — wake all pending waiters with errors
            let mut pending = reader_inner.pending.write().await;
            for (id, waiter) in pending.drain() {
                let _ = waiter.send(ChannelResponse::err(
                    id,
                    ChannelErrorCode::Internal,
                    "Channel closed",
                ));
            }
        });

        let client = Self { inner };
        (client, event_rx)
    }

    // ── Low-level request/response ─────────────────────────────────────

    /// Make a typed call and get back exactly that call's output.
    ///
    /// The return type is inferred from `C`, so a call and its reply cannot
    /// drift apart: pairing them is the [`PlatformCall`] impl's job, and
    /// that impl is generated alongside both wire enums.
    pub async fn call<C: PlatformCall>(&self, call: C) -> Result<C::Output, PluginSdkError> {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);

        let (tx, rx) = oneshot::channel();
        self.inner.pending.write().await.insert(id, tx);

        let msg = ChannelMessage::Request(ChannelRequest {
            id,
            call: call.into_request(),
        });

        self.inner
            .tx
            .send(msg)
            .await
            .map_err(|_| PluginSdkError::ChannelClosed)?;

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
                let event: temps_core::external_plugin::PluginEvent =
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
