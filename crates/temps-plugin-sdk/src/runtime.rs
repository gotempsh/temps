// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Plugin binary runtime — handles startup, handshake, and serving.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use clap::Parser;
use hyper_util::rt::TokioIo;
use temps_core::external_plugin::{PluginEvent, PLUGIN_CHANNEL_PATH, PLUGIN_EVENTS_PATH};
use tokio::io::AsyncBufReadExt;
use tokio::net::UnixListener;
use tower::{Service, ServiceExt};
use tracing::{debug, error, info, warn};

use crate::client::{EventReceiver, TempsClient};
use crate::context::PluginContext;
use crate::manifest::{
    HandshakeMessage, PluginHello, PluginLaunchConfig, PluginReady,
    EXTERNAL_PLUGIN_PROTOCOL_VERSION,
};
use crate::protocol::PluginArgs;
use crate::ExternalPlugin;

/// Run an external plugin. Called by the `main!` macro.
///
/// This function:
/// 1. Parses CLI args
/// 2. Sets up tracing
/// 3. Emits the protocol hello and manifest
/// 4. Reads the typed launch configuration from stdin
/// 5. Starts axum on the Unix socket and emits Ready
/// 6. Authenticates the platform channel and creates the PluginContext
/// 7. Serves plugin routes and events until SIGTERM
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
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("Failed to build plugin runtime: {error}");
            std::process::exit(1);
        }
    };

    rt.block_on(async move {
        if let Err(e) = run_plugin_async(plugin, args).await {
            error!("Plugin failed: {}", e);
            std::process::exit(1);
        }
    });
}

fn write_handshake_message(message: &HandshakeMessage) -> Result<(), crate::error::PluginSdkError> {
    let json = serde_json::to_string(message)?;
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    writeln!(output, "{json}")?;
    output.flush()?;
    Ok(())
}

async fn run_plugin_async<P: ExternalPlugin>(
    plugin: P,
    mut args: PluginArgs,
) -> Result<(), crate::error::PluginSdkError> {
    let manifest = plugin.manifest();
    let plugin_name = manifest.name.clone();

    info!(plugin = %plugin_name, "Starting external plugin");

    // Step 1: identify the protocol and disclose requested privileges before
    // Temps sends any secret or host-owned path to this same child process.
    // IMPORTANT: stdout is ONLY for handshake messages. Logs go to stderr.
    write_handshake_message(&HandshakeMessage::Hello(PluginHello {
        protocol_version: EXTERNAL_PLUGIN_PROTOCOL_VERSION,
        manifest: Box::new(manifest.clone()),
    }))?;

    // Step 2: receive the typed launch configuration. The manager fills only
    // fields the manifest requested. Installed plugins execute as trusted host
    // code today; this staged exchange is disclosure control and protocol
    // integrity, not process isolation.
    let mut launch_line = String::new();
    let mut stdin = tokio::io::BufReader::new(tokio::io::stdin());
    stdin.read_line(&mut launch_line).await.map_err(|error| {
        crate::error::PluginSdkError::Initialization {
            plugin_name: plugin_name.clone(),
            reason: format!("Failed to read typed launch configuration: {error}"),
        }
    })?;
    if launch_line.is_empty() {
        return Err(crate::error::PluginSdkError::Initialization {
            plugin_name,
            reason: "Temps closed stdin before sending typed launch configuration; this plugin and Temps likely use incompatible SDK versions".to_string(),
        });
    }
    let launch: PluginLaunchConfig = serde_json::from_str(launch_line.trim_end())?;
    if launch.protocol_version != EXTERNAL_PLUGIN_PROTOCOL_VERSION {
        return Err(crate::error::PluginSdkError::Initialization {
            plugin_name,
            reason: format!(
                "Temps sent external-plugin protocol {}, but this SDK requires {}",
                launch.protocol_version, EXTERNAL_PLUGIN_PROTOCOL_VERSION
            ),
        });
    }
    if launch.auth_secret.trim().is_empty() {
        return Err(crate::error::PluginSdkError::Initialization {
            plugin_name,
            reason: "Temps supplied an empty request-assertion secret".to_string(),
        });
    }
    if manifest.requires_db != launch.database_url.is_some()
        || manifest.requires_host_data_access != launch.host_data_dir.is_some()
    {
        return Err(crate::error::PluginSdkError::Initialization {
            plugin_name,
            reason: "Temps launch configuration does not match the manifest's declared host access requirements".to_string(),
        });
    }
    let auth_secret = launch.auth_secret;
    args.auth_secret = Some(auth_secret.clone());
    args.database_url = launch.database_url;
    args.host_data_dir = launch.host_data_dir;

    // Step 3: Ensure data directory exists
    let data_dir = PathBuf::from(&args.data_dir);
    tokio::fs::create_dir_all(&data_dir).await.map_err(|e| {
        crate::error::PluginSdkError::Initialization {
            plugin_name: plugin_name.clone(),
            reason: format!("Failed to create data dir {}: {}", data_dir.display(), e),
        }
    })?;

    // Step 4: Wrap plugin in Arc for shared access
    let plugin = Arc::new(plugin);

    // Step 5: Remove stale socket file if it exists
    let socket_path = PathBuf::from(&args.socket_path);
    if socket_path.exists() {
        tokio::fs::remove_file(&socket_path).await.map_err(|e| {
            crate::error::PluginSdkError::SocketBind {
                path: args.socket_path.clone(),
                reason: format!("Failed to remove stale socket: {}", e),
            }
        })?;
    }

    // Step 6: Bind Unix socket
    let listener =
        UnixListener::bind(&socket_path).map_err(|e| crate::error::PluginSdkError::SocketBind {
            path: args.socket_path.clone(),
            reason: e.to_string(),
        })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o600)).map_err(
            |error| crate::error::PluginSdkError::SocketBind {
                path: args.socket_path.clone(),
                reason: format!("Failed to restrict socket permissions to 0600: {error}"),
            },
        )?;
    }

    info!(
        plugin = %plugin_name,
        socket = %args.socket_path,
        "Plugin server listening on Unix socket"
    );

    // Step 7: Signal ready to Temps (handshake phase 2)
    // Include OpenAPI schema if the plugin provides one
    let openapi_json = plugin
        .openapi_schema()
        .and_then(|schema| serde_json::to_value(&schema).ok());
    let ready_msg = HandshakeMessage::Ready(PluginReady {
        ready: true,
        has_ui: plugin.ui_assets().is_some(),
        protocol_version: EXTERNAL_PLUGIN_PROTOCOL_VERSION,
        openapi: openapi_json,
    });
    write_handshake_message(&ready_msg)?;

    // Step 8: Prepare the authenticated platform WebSocket channel.
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
    let channel_secret = auth_secret.clone();
    let channel_route = get(
        move |headers: HeaderMap, ws: axum::extract::WebSocketUpgrade| {
            let ws_tx = ws_tx_clone.clone();
            let pname = channel_handler_plugin_name.clone();
            let expected_secret = channel_secret.clone();
            async move {
                let supplied_secret = headers
                    .get(crate::protocol::headers::AUTH_SIGNATURE)
                    .and_then(|value| value.to_str().ok());
                if !supplied_secret.is_some_and(|provided| {
                    crate::auth::secure_secret_matches(provided, &expected_secret)
                }) {
                    warn!(plugin = %pname, "Rejected unauthenticated platform channel attempt");
                    return StatusCode::UNAUTHORIZED.into_response();
                }

                let sender = {
                    let mut guard = ws_tx.lock().await;
                    match guard.take() {
                        Some(sender) => sender,
                        None => return StatusCode::CONFLICT.into_response(),
                    }
                };

                ws.on_upgrade(move |socket| async move {
                    debug!(plugin = %pname, "Platform channel WebSocket connected");

                    // Convert axum WebSocket to tokio-tungstenite compatible stream
                    let ws_stream = AxumWsAdapter::new(socket);
                    let (client, event_rx) = TempsClient::from_ws(ws_stream);

                    // Send the client to the main task (only the first connection wins)
                    let _ = sender.send((client, event_rx));
                })
                .into_response()
            }
        },
    );

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

    // Step 9: Wait for the platform channel to connect (with timeout)
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

    // Step 10: Build the PluginContext with the TempsClient
    let ctx = PluginContext::new(
        temps_client,
        plugin_name.clone(),
        data_dir,
        args.database_url.clone(),
        args.host_data_dir.as_deref().map(std::path::PathBuf::from),
        args.host_api_url.clone(),
        auth_secret.clone(),
    );

    // Step 11: Call on_start hook
    plugin.on_start(&ctx)?;

    // Step 12: Build the full router with plugin routes
    let plugin_router = plugin.router(ctx.clone()).layer(axum::middleware::from_fn({
        let auth_secret = auth_secret.clone();
        move |mut request: axum::extract::Request, next: axum::middleware::Next| {
            let auth_secret = auth_secret.clone();
            async move {
                match crate::auth::verify_proxy_headers(&mut request, &auth_secret) {
                    Ok(_) => next.run(request).await,
                    Err(rejection) => rejection.into_response(),
                }
            }
        }
    }));

    // Build the events handler if needed
    let event_state = EventHandlerState {
        plugin: plugin.clone(),
        ctx: ctx.clone(),
        auth_secret: auth_secret.clone(),
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

    // Step 13: Spawn event delivery task (events received via channel)
    let event_plugin = plugin.clone();
    let event_ctx = ctx.clone();
    let event_plugin_name = plugin_name.clone();
    spawn_event_delivery(event_rx, event_plugin, event_ctx, event_plugin_name);

    // Step 14: Wait for shutdown
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
    auth_secret: String,
}

// Manual Clone impl — Arc<P> is always Clone regardless of P's Clone impl.
impl<P: ExternalPlugin> Clone for EventHandlerState<P> {
    fn clone(&self) -> Self {
        Self {
            plugin: self.plugin.clone(),
            ctx: self.ctx.clone(),
            auth_secret: self.auth_secret.clone(),
        }
    }
}

/// Handler for `POST /_events` — receives platform events from Temps.
/// (Kept for backward compatibility with the HTTP-based event delivery.)
async fn event_handler<P: ExternalPlugin>(
    State(state): State<EventHandlerState<P>>,
    headers: HeaderMap,
    Json(event): Json<PluginEvent>,
) -> Response {
    let supplied_secret = headers
        .get(crate::protocol::headers::AUTH_SIGNATURE)
        .and_then(|value| value.to_str().ok());
    if !supplied_secret
        .is_some_and(|provided| crate::auth::secure_secret_matches(provided, &state.auth_secret))
    {
        warn!(plugin = %state.ctx.plugin_name(), "Rejected unauthenticated internal event");
        return StatusCode::UNAUTHORIZED.into_response();
    }
    debug!(
        plugin = %state.ctx.plugin_name(),
        event_type = %event.event_type,
        event_id = %event.id,
        "Received platform event"
    );

    state.plugin.on_event(&state.ctx, event);

    StatusCode::OK.into_response()
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

/// Pull the actor token and its user out of the platform's headers.
///
/// Both must be present and agree: a token with no user id cannot be filed
/// under anyone, and a user id with no token is nothing to remember. Absent
/// on `public_paths` routes and on plugins that declare no API capability,
/// which is why this silently does nothing rather than warning.
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
