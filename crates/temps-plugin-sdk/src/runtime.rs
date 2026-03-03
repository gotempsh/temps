//! Plugin binary runtime — handles startup, handshake, and serving.

use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use clap::Parser;
use hyper_util::rt::TokioIo;
use temps_core::external_plugin::{PluginEvent, PLUGIN_CHANNEL_PATH, PLUGIN_EVENTS_PATH};
use tokio::net::UnixListener;
use tower::{Service, ServiceExt};
use tracing::{debug, error, info, warn};

use crate::client::{EventReceiver, TempsClient};
use crate::context::PluginContext;
use crate::manifest::{HandshakeMessage, PluginReady};
use crate::protocol::PluginArgs;
use crate::ExternalPlugin;

/// Run an external plugin. Called by the `main!` macro.
///
/// This function:
/// 1. Parses CLI args
/// 2. Sets up tracing
/// 3. Writes the manifest to stdout (handshake phase 1)
/// 4. Starts axum on the Unix socket with a WebSocket endpoint for the platform channel
/// 5. Writes the ready signal to stdout (handshake phase 2)
/// 6. Waits for the platform to connect via WebSocket, then creates the PluginContext
/// 7. Serves until SIGTERM
pub fn run_plugin<P: ExternalPlugin + Default>(plugin: P) {
    // Parse CLI arguments
    let args = PluginArgs::parse();

    // Set up tracing with JSON output
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .json()
        .with_target(true)
        .with_writer(std::io::stderr) // Write logs to stderr, not stdout (stdout is for handshake)
        .init();

    // Build tokio runtime
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("Failed to build tokio runtime");

    rt.block_on(async move {
        if let Err(e) = run_plugin_async(plugin, args).await {
            error!("Plugin failed: {}", e);
            std::process::exit(1);
        }
    });
}

async fn run_plugin_async<P: ExternalPlugin>(
    plugin: P,
    args: PluginArgs,
) -> Result<(), crate::error::PluginSdkError> {
    let manifest = plugin.manifest();
    let plugin_name = manifest.name.clone();

    info!(plugin = %plugin_name, "Starting external plugin");

    // Step 1: Write manifest to stdout (handshake phase 1)
    // IMPORTANT: stdout is ONLY for handshake messages. Logs go to stderr.
    let manifest_msg = HandshakeMessage::Manifest(Box::new(manifest.clone()));
    let manifest_json = serde_json::to_string(&manifest_msg)?;
    println!("{}", manifest_json);

    // Step 2: Ensure data directory exists
    let data_dir = PathBuf::from(&args.data_dir);
    tokio::fs::create_dir_all(&data_dir).await.map_err(|e| {
        crate::error::PluginSdkError::Initialization {
            plugin_name: plugin_name.clone(),
            reason: format!("Failed to create data dir {}: {}", data_dir.display(), e),
        }
    })?;

    // Step 3: Wrap plugin in Arc for shared access
    let plugin = Arc::new(plugin);

    // Step 4: Remove stale socket file if it exists
    let socket_path = PathBuf::from(&args.socket_path);
    if socket_path.exists() {
        tokio::fs::remove_file(&socket_path).await.map_err(|e| {
            crate::error::PluginSdkError::SocketBind {
                path: args.socket_path.clone(),
                reason: format!("Failed to remove stale socket: {}", e),
            }
        })?;
    }

    // Step 5: Bind Unix socket
    let listener =
        UnixListener::bind(&socket_path).map_err(|e| crate::error::PluginSdkError::SocketBind {
            path: args.socket_path.clone(),
            reason: e.to_string(),
        })?;

    info!(
        plugin = %plugin_name,
        socket = %args.socket_path,
        "Plugin server listening on Unix socket"
    );

    // Step 6: Signal ready to Temps (handshake phase 2)
    // Include OpenAPI schema if the plugin provides one
    let openapi_json = plugin
        .openapi_schema()
        .and_then(|schema| serde_json::to_value(&schema).ok());
    let ready_msg = HandshakeMessage::Ready(PluginReady {
        ready: true,
        has_ui: plugin.ui_assets().is_some(),
        openapi: openapi_json,
    });
    let ready_json = serde_json::to_string(&ready_msg)?;
    println!("{}", ready_json);

    // Step 7: Wait for the platform to connect via WebSocket on /_temps/channel.
    //
    // We use a oneshot channel: the first request to /_temps/channel
    // upgrades to WebSocket and sends the stream here, which we use
    // to build the TempsClient.
    let (ws_tx, ws_rx) = tokio::sync::oneshot::channel::<(TempsClient, EventReceiver)>();
    let ws_tx = Arc::new(tokio::sync::Mutex::new(Some(ws_tx)));

    // Build the initial router (health + channel endpoint only).
    // Plugin routes are added once the channel is established.
    let subscribed_events = manifest.events.clone();
    let has_event_subscriptions = !subscribed_events.is_empty();

    let ws_tx_clone = ws_tx.clone();
    let channel_handler_plugin_name = plugin_name.clone();
    let channel_route = get(move |ws: axum::extract::WebSocketUpgrade| {
        let ws_tx = ws_tx_clone.clone();
        let pname = channel_handler_plugin_name.clone();
        async move {
            ws.on_upgrade(move |socket| async move {
                debug!(plugin = %pname, "Platform channel WebSocket connected");

                // Convert axum WebSocket to tokio-tungstenite compatible stream
                let ws_stream = AxumWsAdapter::new(socket);
                let (client, event_rx) = TempsClient::from_ws(ws_stream);

                // Send the client to the main task (only the first connection wins)
                let mut guard = ws_tx.lock().await;
                if let Some(tx) = guard.take() {
                    let _ = tx.send((client, event_rx));
                }
            })
        }
    });

    let initial_app = Router::new()
        .route(&manifest.health_path, get(health_handler))
        .route(PLUGIN_CHANNEL_PATH, channel_route);

    // Serve the initial app (health + channel) while waiting for the channel to connect
    let app_state: Arc<tokio::sync::RwLock<Option<Router>>> =
        Arc::new(tokio::sync::RwLock::new(None));
    let app_state_clone = app_state.clone();
    let initial_app_clone = initial_app.clone();

    // Build the combined app that delegates to either the initial or full router
    let serve_app = Router::new().fallback(move |request: axum::extract::Request| {
        let app_state = app_state_clone.clone();
        let initial_app = initial_app_clone.clone();
        async move {
            let full_app = app_state.read().await;
            if let Some(ref router) = *full_app {
                router.clone().oneshot(request).await.into_response()
            } else {
                initial_app.clone().oneshot(request).await.into_response()
            }
        }
    });

    // Spawn the listener task
    let shutdown = tokio::signal::ctrl_c();
    tokio::pin!(shutdown);

    let serve_app_for_loop = serve_app.clone();
    let listener_task = tokio::spawn(async move {
        loop {
            tokio::select! {
                accept = listener.accept() => {
                    match accept {
                        Ok((stream, _addr)) => {
                            let tower_service = serve_app_for_loop.clone();
                            tokio::spawn(async move {
                                let socket = TokioIo::new(stream);
                                let hyper_service = hyper::service::service_fn(move |request: hyper::Request<hyper::body::Incoming>| {
                                    let mut tower_service = tower_service.clone();
                                    async move {
                                        tower_service.call(request).await
                                    }
                                });

                                if let Err(err) = hyper_util::server::conn::auto::Builder::new(
                                    hyper_util::rt::TokioExecutor::new()
                                )
                                .serve_connection_with_upgrades(socket, hyper_service)
                                .await
                                {
                                    let err_str = err.to_string();
                                    if !err_str.contains("shutting down") {
                                        error!("Failed to serve connection: {}", err);
                                    }
                                }
                            });
                        }
                        Err(e) => {
                            error!("Failed to accept connection: {}", e);
                        }
                    }
                }
            }
        }
    });

    // Step 8: Wait for the platform channel to connect (with timeout)
    let (temps_client, event_rx) = match tokio::time::timeout(
        std::time::Duration::from_secs(30),
        ws_rx,
    )
    .await
    {
        Ok(Ok((client, event_rx))) => {
            info!(plugin = %plugin_name, "Platform channel established");
            (client, event_rx)
        }
        Ok(Err(_)) => {
            // The sender was dropped without sending — this shouldn't happen
            error!(
                plugin = %plugin_name,
                "Platform channel sender dropped unexpectedly"
            );
            return Err(crate::error::PluginSdkError::Initialization {
                plugin_name: plugin_name.clone(),
                reason: "Platform channel connection failed".to_string(),
            });
        }
        Err(_) => {
            warn!(
                plugin = %plugin_name,
                "Platform did not connect the channel within 30s — running without platform data access"
            );
            // Create a dummy client that will return errors for all calls.
            // This allows the plugin to still serve HTTP routes.
            return Err(crate::error::PluginSdkError::Initialization {
                plugin_name: plugin_name.clone(),
                reason: "Platform channel connection timed out after 30s".to_string(),
            });
        }
    };

    // Step 9: Build the PluginContext with the TempsClient
    let ctx = PluginContext::new(
        temps_client,
        plugin_name.clone(),
        data_dir,
        args.auth_secret.clone(),
    );

    // Step 10: Call on_start hook
    plugin.on_start(&ctx)?;

    // Step 11: Build the full router with plugin routes
    let plugin_router = plugin.router(ctx.clone());

    // Build the events handler if needed
    let event_state = EventHandlerState {
        plugin: plugin.clone(),
        ctx: ctx.clone(),
    };

    let mut full_app = Router::new()
        .route(&manifest.health_path, get(health_handler))
        .merge(plugin_router);

    if has_event_subscriptions {
        info!(
            plugin = %plugin_name,
            events = ?subscribed_events,
            "Plugin subscribes to {} event type(s)",
            subscribed_events.len()
        );
        full_app = full_app.route(
            PLUGIN_EVENTS_PATH,
            post(event_handler::<P>).with_state(event_state.clone()),
        );
    }

    // Swap in the full router
    {
        let mut app_guard = app_state.write().await;
        *app_guard = Some(full_app);
    }

    info!(plugin = %plugin_name, "Plugin fully initialized and serving requests");

    // Step 12: Spawn event delivery task (events received via channel)
    let event_plugin = plugin.clone();
    let event_ctx = ctx.clone();
    let event_plugin_name = plugin_name.clone();
    spawn_event_delivery(event_rx, event_plugin, event_ctx, event_plugin_name);

    // Step 13: Wait for shutdown
    shutdown.await.ok();
    info!(plugin = %plugin_name, "Received shutdown signal");
    plugin.on_shutdown();

    // Cleanup
    listener_task.abort();
    let _ = tokio::fs::remove_file(&socket_path).await;
    info!(plugin = %plugin_name, "Plugin shut down cleanly");

    Ok(())
}

/// Spawn a task that reads events from the channel and calls on_event.
fn spawn_event_delivery<P: ExternalPlugin>(
    mut event_rx: EventReceiver,
    plugin: Arc<P>,
    ctx: PluginContext,
    plugin_name: String,
) {
    tokio::spawn(async move {
        while let Some(event) = event_rx.recv().await {
            debug!(
                plugin = %plugin_name,
                event_type = %event.event_type,
                "Received platform event via channel"
            );
            plugin.on_event(&ctx, event);
        }
        debug!(plugin = %plugin_name, "Event delivery task ended");
    });
}

/// Simple health check handler.
async fn health_handler() -> &'static str {
    "ok"
}

/// Shared state for the `/_events` route handler.
struct EventHandlerState<P: ExternalPlugin> {
    plugin: Arc<P>,
    ctx: PluginContext,
}

// Manual Clone impl — Arc<P> is always Clone regardless of P's Clone impl.
impl<P: ExternalPlugin> Clone for EventHandlerState<P> {
    fn clone(&self) -> Self {
        Self {
            plugin: self.plugin.clone(),
            ctx: self.ctx.clone(),
        }
    }
}

/// Handler for `POST /_events` — receives platform events from Temps.
/// (Kept for backward compatibility with the HTTP-based event delivery.)
async fn event_handler<P: ExternalPlugin>(
    State(state): State<EventHandlerState<P>>,
    Json(event): Json<PluginEvent>,
) -> impl IntoResponse {
    debug!(
        plugin = %state.ctx.plugin_name(),
        event_type = %event.event_type,
        event_id = %event.id,
        "Received platform event"
    );

    state.plugin.on_event(&state.ctx, event);

    StatusCode::OK
}

// ── Axum WebSocket → tokio-tungstenite adapter ─────────────────────────
//
// The TempsClient::from_ws expects a futures Stream+Sink of tungstenite
// Messages.  Axum's WebSocket gives us a different type, so we adapt it.

use std::pin::Pin;
use std::task::{Context, Poll};

struct AxumWsAdapter {
    inner: axum::extract::ws::WebSocket,
}

impl AxumWsAdapter {
    fn new(ws: axum::extract::ws::WebSocket) -> Self {
        Self { inner: ws }
    }
}

impl futures::Stream for AxumWsAdapter {
    type Item =
        Result<tokio_tungstenite::tungstenite::Message, tokio_tungstenite::tungstenite::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match Pin::new(&mut self.inner).poll_next(cx) {
            Poll::Ready(Some(Ok(msg))) => {
                let tongue_msg = axum_msg_to_tungstenite(msg);
                Poll::Ready(Some(Ok(tongue_msg)))
            }
            Poll::Ready(Some(Err(e))) => Poll::Ready(Some(Err(
                tokio_tungstenite::tungstenite::Error::Io(std::io::Error::other(e.to_string())),
            ))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl futures::Sink<tokio_tungstenite::tungstenite::Message> for AxumWsAdapter {
    type Error = tokio_tungstenite::tungstenite::Error;

    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        <axum::extract::ws::WebSocket as futures::Sink<axum::extract::ws::Message>>::poll_ready(
            Pin::new(&mut self.inner),
            cx,
        )
        .map_err(|e| {
            tokio_tungstenite::tungstenite::Error::Io(std::io::Error::other(e.to_string()))
        })
    }

    fn start_send(
        mut self: Pin<&mut Self>,
        item: tokio_tungstenite::tungstenite::Message,
    ) -> Result<(), Self::Error> {
        let axum_msg = tungstenite_msg_to_axum(item);
        <axum::extract::ws::WebSocket as futures::Sink<axum::extract::ws::Message>>::start_send(
            Pin::new(&mut self.inner),
            axum_msg,
        )
        .map_err(|e| {
            tokio_tungstenite::tungstenite::Error::Io(std::io::Error::other(e.to_string()))
        })
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        <axum::extract::ws::WebSocket as futures::Sink<axum::extract::ws::Message>>::poll_flush(
            Pin::new(&mut self.inner),
            cx,
        )
        .map_err(|e| {
            tokio_tungstenite::tungstenite::Error::Io(std::io::Error::other(e.to_string()))
        })
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        <axum::extract::ws::WebSocket as futures::Sink<axum::extract::ws::Message>>::poll_close(
            Pin::new(&mut self.inner),
            cx,
        )
        .map_err(|e| {
            tokio_tungstenite::tungstenite::Error::Io(std::io::Error::other(e.to_string()))
        })
    }
}

fn axum_msg_to_tungstenite(
    msg: axum::extract::ws::Message,
) -> tokio_tungstenite::tungstenite::Message {
    match msg {
        axum::extract::ws::Message::Text(t) => {
            tokio_tungstenite::tungstenite::Message::Text(t.to_string().into())
        }
        axum::extract::ws::Message::Binary(b) => {
            tokio_tungstenite::tungstenite::Message::Binary(b.to_vec().into())
        }
        axum::extract::ws::Message::Ping(p) => {
            tokio_tungstenite::tungstenite::Message::Ping(p.to_vec().into())
        }
        axum::extract::ws::Message::Pong(p) => {
            tokio_tungstenite::tungstenite::Message::Pong(p.to_vec().into())
        }
        axum::extract::ws::Message::Close(c) => {
            tokio_tungstenite::tungstenite::Message::Close(c.map(|cf| {
                tokio_tungstenite::tungstenite::protocol::CloseFrame {
                    code: tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode::from(
                        cf.code,
                    ),
                    reason: cf.reason.to_string().into(),
                }
            }))
        }
    }
}

fn tungstenite_msg_to_axum(
    msg: tokio_tungstenite::tungstenite::Message,
) -> axum::extract::ws::Message {
    match msg {
        tokio_tungstenite::tungstenite::Message::Text(t) => {
            axum::extract::ws::Message::Text(t.to_string().into())
        }
        tokio_tungstenite::tungstenite::Message::Binary(b) => {
            axum::extract::ws::Message::Binary(b.to_vec().into())
        }
        tokio_tungstenite::tungstenite::Message::Ping(p) => {
            axum::extract::ws::Message::Ping(p.to_vec().into())
        }
        tokio_tungstenite::tungstenite::Message::Pong(p) => {
            axum::extract::ws::Message::Pong(p.to_vec().into())
        }
        tokio_tungstenite::tungstenite::Message::Close(c) => {
            axum::extract::ws::Message::Close(c.map(|cf| axum::extract::ws::CloseFrame {
                code: cf.code.into(),
                reason: cf.reason.to_string().into(),
            }))
        }
        tokio_tungstenite::tungstenite::Message::Frame(_) => {
            // Raw frames are not exposed by axum — treat as no-op
            axum::extract::ws::Message::Ping(vec![].into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_health_handler_exists() {
        // Verify the handler function exists and is the right type
        let _: fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = &'static str> + Send>> =
            || Box::pin(health_handler());
    }
}
