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
use temps_plugin_protocol::{PluginEvent, PLUGIN_CHANNEL_PATH, PLUGIN_EVENTS_PATH};
use tokio::io::AsyncBufReadExt;
use tokio::net::UnixListener;
use tower::{Service, ServiceExt};
use tracing::{debug, error, info, warn};

use crate::client::{EventReceiver, TempsClient};
use crate::context::PluginContext;
use crate::manifest::{
    HandshakeMessage, PluginHello, PluginLaunchConfig, PluginReady, PluginServiceHello,
    EXTERNAL_PLUGIN_PROTOCOL_VERSION, PLUGIN_HELLO_PATH,
};
use crate::protocol::PluginArgs;
use crate::ExternalPlugin;

/// How this plugin was started.
enum Launch {
    /// A Temps process started it as a child and passed a socket path.
    Process(PluginArgs),
    /// It runs as a Temps service: a container Temps deployed and reaches
    /// over the network.
    Service,
}

/// Whether Temps started this plugin as a child process. A plugin that runs
/// as a service is started by its container with no plugin arguments at all.
fn started_as_child_process(arguments: impl IntoIterator<Item = String>) -> bool {
    arguments
        .into_iter()
        .any(|argument| argument == "--socket-path" || argument.starts_with("--socket-path="))
}

/// Run an external plugin. Called by the `main!` macro.
///
/// A plugin runs in one of two ways, chosen by how it was started.
///
/// **As a Temps service** (no `--socket-path` argument): the plugin is the
/// container of an ordinary Temps deployment. It listens on `PORT`, answers
/// the platform's authenticated hello with its manifest, and serves requests
/// once the platform has connected the channel. See [`ServiceEnvironment`].
///
/// **As a child process** (`--socket-path` given): this function
/// 1. Parses CLI args
/// 2. Sets up tracing
/// 3. Emits the protocol hello and manifest
/// 4. Reads the typed launch configuration from stdin
/// 5. Starts axum on the Unix socket and emits Ready
/// 6. Authenticates the platform channel and creates the PluginContext
/// 7. Serves plugin routes and events until SIGTERM
pub fn run_plugin<P: ExternalPlugin + Default>(plugin: P) {
    let launch = if started_as_child_process(std::env::args()) {
        Launch::Process(PluginArgs::parse())
    } else {
        Launch::Service
    };

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
        let outcome = match launch {
            Launch::Process(args) => run_plugin_async(plugin, args).await,
            Launch::Service => match ServiceEnvironment::from_process_environment() {
                Ok(environment) => run_service_async(plugin, environment).await,
                Err(error) => Err(error),
            },
        };
        if let Err(e) = outcome {
            error!("Plugin failed: {}", e);
            std::process::exit(1);
        }
    });
}

/// What a plugin that runs as a Temps service reads from its container's
/// environment. Temps sets all of it on every deployment; a plugin needs no
/// configuration of its own to be reachable.
#[derive(Clone)]
pub struct ServiceEnvironment {
    /// `PORT`: the port the service must listen on. Defaults to 8080.
    pub port: u16,
    /// `TEMPS_API_TOKEN`: the deployment token. The request-assertion secret
    /// is derived from it; see [`service_auth_secret`].
    api_token: String,
    /// `TEMPS_API_URL` without its `/api` suffix: where the instance answers.
    pub host_api_url: Option<String>,
    /// `TEMPS_PLUGIN_DATA_DIR`, or `/data`: where the plugin keeps its files.
    pub data_dir: PathBuf,
}

impl std::fmt::Debug for ServiceEnvironment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServiceEnvironment")
            .field("port", &self.port)
            .field("api_token", &"[REDACTED]")
            .field("host_api_url", &self.host_api_url)
            .field("data_dir", &self.data_dir)
            .finish()
    }
}

/// Port a plugin service listens on when its environment names none.
pub const DEFAULT_SERVICE_PORT: u16 = 8080;

/// Where a plugin service keeps its files when its environment names no
/// directory. Mount the service's volume here to keep them across deployments.
pub const DEFAULT_SERVICE_DATA_DIR: &str = "/data";

impl ServiceEnvironment {
    #[allow(
        clippy::disallowed_methods,
        reason = "bootstrap: the environment Temps gives a plugin's container is that plugin's only launch input"
    )]
    fn from_process_environment() -> Result<Self, crate::error::PluginSdkError> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    /// Read the service environment through `lookup`, which returns the value
    /// of one variable.
    pub fn from_lookup(
        lookup: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, crate::error::PluginSdkError> {
        let value = |name: &str| {
            lookup(name)
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        };
        let port = match value("PORT") {
            None => DEFAULT_SERVICE_PORT,
            Some(raw) => raw
                .parse::<u16>()
                .ok()
                .filter(|port| *port > 0)
                .ok_or_else(|| crate::error::PluginSdkError::Args {
                    message: format!(
                        "PORT is '{raw}', which is not a TCP port between 1 and 65535"
                    ),
                })?,
        };
        let api_token =
            value("TEMPS_API_TOKEN").ok_or_else(|| crate::error::PluginSdkError::Args {
                message: "TEMPS_API_TOKEN is not set. A plugin runs as a Temps service: deploy it \
                          to a Temps instance, which sets this variable on every deployment, and \
                          add the service under Settings > Plugins. Temps and the plugin both \
                          derive the request-assertion secret from it."
                    .to_string(),
            })?;
        let host_api_url = value("TEMPS_API_URL").map(|url| {
            let url = url.trim_end_matches('/');
            url.strip_suffix("/api").unwrap_or(url).to_string()
        });
        let data_dir = PathBuf::from(
            value("TEMPS_PLUGIN_DATA_DIR").unwrap_or_else(|| DEFAULT_SERVICE_DATA_DIR.to_string()),
        );
        Ok(Self {
            port,
            api_token,
            host_api_url,
            data_dir,
        })
    }

    /// The secret Temps presents with every request to this service.
    pub fn auth_secret(&self) -> String {
        service_auth_secret(&self.api_token)
    }
}

/// The request-assertion secret of a plugin that runs as a Temps service:
/// the lowercase hex SHA-256 of [`temps_plugin_protocol::SERVICE_AUTH_CONTEXT`],
/// a NUL byte and the service's deployment token.
///
/// Temps computes the same value from the token it issued, so nothing has to
/// be exchanged, and the token itself is never sent to the plugin's port.
pub fn service_auth_secret(api_token: &str) -> String {
    use sha2::Digest as _;
    use std::fmt::Write as _;

    let mut hasher = sha2::Sha256::new();
    hasher.update(temps_plugin_protocol::SERVICE_AUTH_CONTEXT.as_bytes());
    hasher.update([0u8]);
    hasher.update(api_token.as_bytes());
    let digest = hasher.finalize();
    let mut secret = String::with_capacity(digest.len() * 2);
    for byte in digest.iter() {
        let _ = write!(secret, "{byte:02x}");
    }
    secret
}

fn write_handshake_message(message: &HandshakeMessage) -> Result<(), crate::error::PluginSdkError> {
    let json = serde_json::to_string(message)?;
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    writeln!(output, "{json}")?;
    output.flush()?;
    Ok(())
}

/// Run a plugin as a Temps service: listen on the service port and serve the
/// platform once it has connected.
async fn run_service_async<P: ExternalPlugin>(
    plugin: P,
    environment: ServiceEnvironment,
) -> Result<(), crate::error::PluginSdkError> {
    let address = std::net::SocketAddr::from(([0, 0, 0, 0], environment.port));
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .map_err(|error| crate::error::PluginSdkError::SocketBind {
            path: address.to_string(),
            reason: error.to_string(),
        })?;
    serve_service(plugin, listener, environment).await
}

/// Serve a plugin as a Temps service on a listener the caller already bound.
///
/// [`run_plugin`] does this on the port its environment names. Call it
/// directly to run a plugin inside another program, or in a test that plays
/// the platform against it: bind `127.0.0.1:0`, build the
/// [`ServiceEnvironment`] with [`ServiceEnvironment::from_lookup`], and
/// connect with the secret [`ServiceEnvironment::auth_secret`] returns. The
/// listener's port is used; `environment.port` is not.
///
/// Returns when the process is told to stop.
pub async fn serve_service<P: ExternalPlugin>(
    plugin: P,
    listener: tokio::net::TcpListener,
    environment: ServiceEnvironment,
) -> Result<(), crate::error::PluginSdkError> {
    let manifest = plugin.manifest();
    let plugin_name = manifest.name.clone();

    // A service is never given the control-plane database or the instance
    // data directory. Say so here instead of starting a plugin that would
    // fail on its first query.
    if manifest.requires_db || manifest.requires_host_data_access {
        return Err(crate::error::PluginSdkError::Initialization {
            plugin_name,
            reason: "the manifest asks for the control-plane database or the instance data \
                     directory, which a plugin running as a Temps service never receives. Remove \
                     requires_db and requires_host_data_access; link a database to the service \
                     for the plugin's own data and use the platform API for everything else."
                .to_string(),
        });
    }

    let data_dir = service_data_dir(&plugin_name, &environment.data_dir).await;
    match listener.local_addr() {
        Ok(address) => {
            info!(plugin = %plugin_name, %address, "Plugin service listening")
        }
        Err(error) => {
            warn!(plugin = %plugin_name, %error, "Plugin service listening on an unknown address")
        }
    }

    serve(
        Arc::new(plugin),
        manifest,
        PluginListener::Tcp(listener),
        ServeConfig {
            auth_secret: environment.auth_secret(),
            data_dir,
            database_url: None,
            host_data_dir: None,
            host_api_url: environment.host_api_url,
            service: true,
        },
    )
    .await
}

/// The directory a plugin service keeps its files in.
///
/// The configured directory is used when it can be created. Otherwise the
/// plugin still starts, on a temporary directory that does not survive the
/// container, and says so: a plugin that stores nothing should not fail to
/// start because no volume is mounted.
async fn service_data_dir(plugin_name: &str, configured: &std::path::Path) -> PathBuf {
    match tokio::fs::create_dir_all(configured).await {
        Ok(()) => configured.to_path_buf(),
        Err(error) => {
            let fallback = std::env::temp_dir().join("temps-plugin-data");
            warn!(
                plugin = %plugin_name,
                configured = %configured.display(),
                fallback = %fallback.display(),
                %error,
                "The plugin's data directory cannot be created; using a temporary directory that \
                 is lost when the container is replaced. Mount a volume at the configured \
                 directory, or set TEMPS_PLUGIN_DATA_DIR, to keep the plugin's files."
            );
            if let Err(error) = tokio::fs::create_dir_all(&fallback).await {
                warn!(plugin = %plugin_name, %error, "The temporary data directory cannot be created either");
            }
            fallback
        }
    }
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

    let served = serve(
        plugin,
        manifest,
        PluginListener::Unix(listener),
        ServeConfig {
            auth_secret,
            data_dir,
            database_url: args.database_url.clone(),
            host_data_dir: args.host_data_dir.as_deref().map(PathBuf::from),
            host_api_url: args.host_api_url.clone(),
            service: false,
        },
    )
    .await;
    let _ = tokio::fs::remove_file(&socket_path).await;
    served
}

/// A byte stream the plugin serves HTTP on.
trait PluginConnection: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send> PluginConnection for T {}

/// Where the plugin accepts the platform's connections.
enum PluginListener {
    /// The private socket of a plugin started as a child process.
    Unix(UnixListener),
    /// The service port of a plugin that runs as a Temps service.
    Tcp(tokio::net::TcpListener),
}

impl PluginListener {
    async fn accept(&self) -> std::io::Result<Box<dyn PluginConnection>> {
        match self {
            Self::Unix(listener) => {
                let (stream, _) = listener.accept().await?;
                Ok(Box::new(stream))
            }
            Self::Tcp(listener) => {
                let (stream, _) = listener.accept().await?;
                Ok(Box::new(stream))
            }
        }
    }
}

/// What the serving loop needs that differs between the two ways a plugin is
/// started.
struct ServeConfig {
    auth_secret: String,
    data_dir: PathBuf,
    database_url: Option<String>,
    host_data_dir: Option<PathBuf>,
    host_api_url: Option<String>,
    /// The plugin runs as a Temps service: it answers the authenticated
    /// hello, waits for the platform for as long as it takes, and accepts a
    /// new channel when the platform reconnects.
    service: bool,
}

/// Resolves when the plugin is asked to stop: an interrupt, or the SIGTERM a
/// container runtime sends before it kills the container.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::terminate()) {
            Ok(mut terminate) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = terminate.recv() => {}
                }
            }
            Err(error) => {
                warn!(%error, "Cannot listen for SIGTERM; only an interrupt stops the plugin cleanly");
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// Whether `headers` carry the request-assertion secret.
fn presents_secret(headers: &HeaderMap, expected_secret: &str) -> bool {
    headers
        .get(crate::protocol::headers::AUTH_SIGNATURE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|provided| crate::auth::secure_secret_matches(provided, expected_secret))
}

/// Serve the plugin on `listener` until it is asked to stop.
async fn serve<P: ExternalPlugin>(
    plugin: Arc<P>,
    manifest: crate::manifest::PluginManifest,
    listener: PluginListener,
    config: ServeConfig,
) -> Result<(), crate::error::PluginSdkError> {
    let plugin_name = manifest.name.clone();
    let auth_secret = config.auth_secret.clone();
    let service = config.service;

    // Prepare the authenticated platform WebSocket channel.
    //
    // The client exists before the platform connects. An authenticated
    // request to /_temps/channel upgrades to WebSocket and becomes the
    // client's connection. A child process accepts exactly one; a service
    // accepts a new one whenever the platform reconnects.
    let (temps_client, event_rx) = TempsClient::detached();
    let connected = Arc::new(tokio::sync::Notify::new());
    let attached_once = Arc::new(std::sync::atomic::AtomicBool::new(false));

    // Build the initial router (health + channel endpoint only).
    // Plugin routes are added once the channel is established.
    let subscribed_events = manifest.events.clone();
    let has_event_subscriptions = !subscribed_events.is_empty();

    let channel_route = {
        let client = temps_client.clone();
        let connected = connected.clone();
        let attached_once = attached_once.clone();
        let plugin_name = plugin_name.clone();
        let expected_secret = auth_secret.clone();
        get(
            move |headers: HeaderMap, ws: axum::extract::WebSocketUpgrade| {
                let client = client.clone();
                let connected = connected.clone();
                let attached_once = attached_once.clone();
                let pname = plugin_name.clone();
                let expected_secret = expected_secret.clone();
                async move {
                    if !presents_secret(&headers, &expected_secret) {
                        warn!(plugin = %pname, "Rejected unauthenticated platform channel attempt");
                        return StatusCode::UNAUTHORIZED.into_response();
                    }
                    let reconnecting =
                        attached_once.swap(true, std::sync::atomic::Ordering::SeqCst);
                    if reconnecting && !service {
                        return StatusCode::CONFLICT.into_response();
                    }

                    ws.on_upgrade(move |socket| async move {
                        debug!(plugin = %pname, reconnecting, "Platform channel WebSocket connected");
                        // Convert axum WebSocket to tokio-tungstenite compatible stream
                        client.attach(AxumWsAdapter::new(socket));
                        connected.notify_one();
                    })
                    .into_response()
                }
            },
        )
    };

    let mut platform_routes = Router::new()
        .route(&manifest.health_path, get(health_handler))
        .route(PLUGIN_CHANNEL_PATH, channel_route);
    if service {
        let openapi = plugin
            .openapi_schema()
            .and_then(|schema| serde_json::to_value(&schema).ok());
        let hello = Arc::new(PluginServiceHello {
            protocol_version: EXTERNAL_PLUGIN_PROTOCOL_VERSION,
            manifest: Box::new(manifest.clone()),
            has_ui: plugin.ui_assets().is_some(),
            openapi,
        });
        let expected_secret = auth_secret.clone();
        let hello_plugin_name = plugin_name.clone();
        platform_routes = platform_routes.route(
            PLUGIN_HELLO_PATH,
            get(move |headers: HeaderMap| {
                let hello = hello.clone();
                let expected_secret = expected_secret.clone();
                let pname = hello_plugin_name.clone();
                async move {
                    if !presents_secret(&headers, &expected_secret) {
                        warn!(plugin = %pname, "Rejected unauthenticated plugin hello");
                        return StatusCode::UNAUTHORIZED.into_response();
                    }
                    Json(hello.as_ref().clone()).into_response()
                }
            }),
        );
    }
    let initial_app = platform_routes.clone();

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
    let shutdown = shutdown_signal();
    tokio::pin!(shutdown);

    let serve_app_for_loop = serve_app.clone();
    let listener_task = tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok(stream) => {
                    let tower_service = serve_app_for_loop.clone();
                    tokio::spawn(async move {
                        let socket = TokioIo::new(stream);
                        let hyper_service = hyper::service::service_fn(
                            move |request: hyper::Request<hyper::body::Incoming>| {
                                let mut tower_service = tower_service.clone();
                                async move { tower_service.call(request).await }
                            },
                        );

                        if let Err(err) = hyper_util::server::conn::auto::Builder::new(
                            hyper_util::rt::TokioExecutor::new(),
                        )
                        .serve_connection_with_upgrades(socket, hyper_service)
                        .await
                        {
                            let err_str = err.to_string();
                            if !err_str.contains("shutting down") {
                                debug!("Failed to serve connection: {}", err);
                            }
                        }
                    });
                }
                Err(e) => {
                    error!("Failed to accept connection: {}", e);
                }
            }
        }
    });

    // Wait for the platform channel to connect. A child process was started
    // by the platform, which connects at once or never. A service is started
    // by its container and waits until the platform attaches to it.
    if service {
        tokio::select! {
            _ = connected.notified() => {}
            _ = &mut shutdown => {
                info!(plugin = %plugin_name, "Received shutdown signal before the platform connected");
                listener_task.abort();
                return Ok(());
            }
        }
    } else if tokio::time::timeout(std::time::Duration::from_secs(30), connected.notified())
        .await
        .is_err()
    {
        warn!(
            plugin = %plugin_name,
            "Platform did not connect the channel within 30s — running without platform data access"
        );
        listener_task.abort();
        return Err(crate::error::PluginSdkError::Initialization {
            plugin_name: plugin_name.clone(),
            reason: "Platform channel connection timed out after 30s".to_string(),
        });
    }
    info!(plugin = %plugin_name, "Platform channel established");

    // Build the PluginContext with the TempsClient
    let ctx = PluginContext::new(
        temps_client,
        plugin_name.clone(),
        config.data_dir,
        config.database_url,
        config.host_data_dir,
        config.host_api_url,
        auth_secret.clone(),
    );

    // Call on_start hook
    plugin.on_start(&ctx)?;

    // Build the full router with plugin routes
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

    // A service keeps answering the hello and accepting the channel, so the
    // platform can attach again after it restarts.
    let mut full_app = if service {
        platform_routes.merge(plugin_router)
    } else {
        Router::new()
            .route(&manifest.health_path, get(health_handler))
            .merge(plugin_router)
    };

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

    // Spawn event delivery task (events received via channel)
    let event_plugin = plugin.clone();
    let event_ctx = ctx.clone();
    let event_plugin_name = plugin_name.clone();
    spawn_event_delivery(event_rx, event_plugin, event_ctx, event_plugin_name);

    // Wait for shutdown
    shutdown.await;
    info!(plugin = %plugin_name, "Received shutdown signal");
    plugin.on_shutdown();

    // Cleanup
    listener_task.abort();
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
// TempsClient::attach expects a futures Stream+Sink of tungstenite
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

    use futures::{SinkExt as _, StreamExt as _};
    use std::sync::atomic::{AtomicBool, Ordering};
    use temps_plugin_protocol::channel::{ChannelMessage, ChannelResponse, PlatformCallResponse};
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::net::TcpStream;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
    use tokio_tungstenite::tungstenite::Message;
    use tokio_tungstenite::WebSocketStream;

    fn environment(pairs: &[(&str, &str)]) -> Result<ServiceEnvironment, PluginSdkErrorText> {
        let pairs: Vec<(String, String)> = pairs
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect();
        ServiceEnvironment::from_lookup(|name| {
            pairs
                .iter()
                .find(|(candidate, _)| candidate == name)
                .map(|(_, value)| value.clone())
        })
        .map_err(|error| PluginSdkErrorText(error.to_string()))
    }

    #[derive(Debug)]
    struct PluginSdkErrorText(String);

    #[test]
    fn a_child_process_is_recognised_by_its_socket_argument() {
        let arguments = |list: &[&str]| list.iter().map(|a| a.to_string()).collect::<Vec<_>>();
        assert!(started_as_child_process(arguments(&[
            "plugin",
            "--socket-path",
            "/run/p.sock",
            "--data-dir",
            "/data"
        ])));
        assert!(started_as_child_process(arguments(&[
            "plugin",
            "--socket-path=/run/p.sock"
        ])));
        assert!(!started_as_child_process(arguments(&["plugin"])));
        assert!(!started_as_child_process(arguments(&[
            "plugin",
            "--verbose"
        ])));
    }

    #[test]
    fn the_service_environment_is_what_temps_sets_on_a_deployment() {
        let environment = environment(&[
            ("PORT", "3000"),
            ("TEMPS_API_TOKEN", "dt_example"),
            ("TEMPS_API_URL", "https://temps.example.com/api/"),
        ])
        .expect("a deployment's environment is enough");
        assert_eq!(environment.port, 3000);
        assert_eq!(
            environment.host_api_url.as_deref(),
            Some("https://temps.example.com")
        );
        assert_eq!(environment.data_dir, PathBuf::from("/data"));
        // The secret is stable across both sides: the platform derives the
        // same value from the token it issued.
        assert_eq!(
            environment.auth_secret(),
            "2ec7a454828e34191fc092dbafcb99df2d280ae2dac66df185cfe83666092593"
        );
        assert!(
            !format!("{environment:?}").contains("dt_example"),
            "the deployment token is never printed"
        );
    }

    #[test]
    fn the_service_environment_has_defaults_for_everything_but_the_token() {
        let environment =
            environment(&[("TEMPS_API_TOKEN", "dt_example")]).expect("only the token is required");
        assert_eq!(environment.port, DEFAULT_SERVICE_PORT);
        assert_eq!(environment.host_api_url, None);

        let custom = environment_with_data_dir();
        assert_eq!(custom.data_dir, PathBuf::from("/var/lib/plugin"));
    }

    fn environment_with_data_dir() -> ServiceEnvironment {
        environment(&[
            ("TEMPS_API_TOKEN", "dt_example"),
            ("TEMPS_PLUGIN_DATA_DIR", "/var/lib/plugin"),
        ])
        .expect("a data directory can be named")
    }

    #[test]
    fn a_service_without_a_deployment_token_explains_how_a_plugin_runs() {
        for pairs in [&[][..], &[("TEMPS_API_TOKEN", "   ")][..]] {
            let error = environment(pairs).expect_err("the token is required");
            assert!(
                error.0.contains("TEMPS_API_TOKEN is not set"),
                "{}",
                error.0
            );
            assert!(error.0.contains("Settings > Plugins"), "{}", error.0);
        }
    }

    #[test]
    fn a_service_with_an_unusable_port_names_the_value() {
        for port in ["0", "70000", "http"] {
            let error = environment(&[("PORT", port), ("TEMPS_API_TOKEN", "dt_example")])
                .expect_err("the port is invalid");
            assert!(
                error.0.contains(&format!("PORT is '{port}'")),
                "{}",
                error.0
            );
        }
    }

    #[test]
    fn different_tokens_give_unrelated_secrets() {
        let first = service_auth_secret("dt_first");
        let second = service_auth_secret("dt_second");
        assert_ne!(first, second);
        assert_eq!(first.len(), 64);
        assert!(!first.contains("dt_first"));
    }

    /// A plugin with one route, which also asks the platform for projects.
    #[derive(Default)]
    struct Probe {
        started: Arc<AtomicBool>,
    }

    impl ExternalPlugin for Probe {
        fn manifest(&self) -> crate::manifest::PluginManifest {
            crate::manifest::PluginManifest::builder("probe", "1.2.3").build()
        }

        fn router(&self, ctx: PluginContext) -> Router {
            Router::new()
                .route("/ping", get(|| async { "pong" }))
                .route(
                    "/projects",
                    get(move || {
                        let ctx = ctx.clone();
                        async move {
                            match ctx.temps().list_projects().await {
                                Ok(projects) => projects.len().to_string(),
                                Err(error) => format!("error: {error}"),
                            }
                        }
                    }),
                )
        }

        fn on_start(&self, _ctx: &PluginContext) -> Result<(), crate::error::PluginSdkError> {
            self.started.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    const SECRET: &str = "test-request-assertion-secret";

    struct Served {
        address: std::net::SocketAddr,
        started: Arc<AtomicBool>,
        task: tokio::task::JoinHandle<Result<(), crate::error::PluginSdkError>>,
        _data: tempfile_free::Directory,
    }

    impl Drop for Served {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    /// A unique directory under the system temporary directory, removed on
    /// drop. The SDK has no temporary-directory dependency of its own.
    mod tempfile_free {
        pub struct Directory(pub std::path::PathBuf);

        impl Directory {
            pub fn new(label: &str) -> Self {
                static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                let path = std::env::temp_dir().join(format!(
                    "temps-plugin-sdk-{label}-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                ));
                std::fs::create_dir_all(&path).unwrap();
                Self(path)
            }
        }

        impl Drop for Directory {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }

    async fn serve_probe(service: bool) -> Served {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let plugin = Probe::default();
        let started = plugin.started.clone();
        let manifest = plugin.manifest();
        let data = tempfile_free::Directory::new("probe");
        let config = ServeConfig {
            auth_secret: SECRET.to_string(),
            data_dir: data.0.clone(),
            database_url: None,
            host_data_dir: None,
            host_api_url: None,
            service,
        };
        let task = tokio::spawn(serve(
            Arc::new(plugin),
            manifest,
            PluginListener::Tcp(listener),
            config,
        ));
        Served {
            address,
            started,
            task,
            _data: data,
        }
    }

    /// One request on a fresh connection: the status and the body.
    async fn get_path(
        address: std::net::SocketAddr,
        path: &str,
        secret: Option<&str>,
    ) -> (u16, String) {
        let mut stream = TcpStream::connect(address).await.unwrap();
        let mut request = format!("GET {path} HTTP/1.1\r\nhost: plugin\r\nconnection: close\r\n");
        if let Some(secret) = secret {
            request.push_str(&format!(
                "{}: {secret}\r\n",
                crate::protocol::headers::AUTH_SIGNATURE
            ));
        }
        request.push_str("\r\n");
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).await.unwrap();
        let text = String::from_utf8_lossy(&raw).into_owned();
        let status = text
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse::<u16>().ok())
            .unwrap_or_else(|| panic!("no status line in {text:?}"));
        let body = text
            .split_once("\r\n\r\n")
            .map(|(_, body)| body.to_string())
            .unwrap_or_default();
        (status, body)
    }

    async fn open_channel(
        address: std::net::SocketAddr,
        secret: Option<&str>,
    ) -> Result<WebSocketStream<TcpStream>, tokio_tungstenite::tungstenite::Error> {
        let stream = TcpStream::connect(address).await.unwrap();
        let mut request = format!("ws://{address}{PLUGIN_CHANNEL_PATH}")
            .into_client_request()
            .unwrap();
        if let Some(secret) = secret {
            request.headers_mut().insert(
                crate::protocol::headers::AUTH_SIGNATURE,
                secret.parse().unwrap(),
            );
        }
        tokio_tungstenite::client_async(request, stream)
            .await
            .map(|(socket, _)| socket)
    }

    /// Poll until the plugin's own routes are being served.
    async fn wait_until_serving(address: std::net::SocketAddr) {
        for _ in 0..200 {
            if get_path(address, "/ping", Some(SECRET)).await.0 == 200 {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("the plugin never started serving its routes");
    }

    fn handshake_status(error: tokio_tungstenite::tungstenite::Error) -> u16 {
        match error {
            tokio_tungstenite::tungstenite::Error::Http(response) => response.status().as_u16(),
            other => panic!("expected an HTTP refusal, got {other}"),
        }
    }

    #[tokio::test]
    async fn a_service_answers_its_hello_only_to_the_platform() {
        let served = serve_probe(true).await;

        assert_eq!(
            get_path(served.address, PLUGIN_HELLO_PATH, None).await.0,
            401
        );
        assert_eq!(
            get_path(served.address, PLUGIN_HELLO_PATH, Some("wrong"))
                .await
                .0,
            401
        );

        let (status, body) = get_path(served.address, PLUGIN_HELLO_PATH, Some(SECRET)).await;
        assert_eq!(status, 200);
        let hello: PluginServiceHello = serde_json::from_str(&body).unwrap();
        assert_eq!(hello.protocol_version, EXTERNAL_PLUGIN_PROTOCOL_VERSION);
        assert_eq!(hello.manifest.name, "probe");
        assert_eq!(hello.manifest.version, "1.2.3");
        assert!(!hello.has_ui);

        // The deployment health check needs no secret.
        assert_eq!(
            get_path(served.address, "/health", None).await,
            (200, "ok".to_string())
        );
    }

    #[tokio::test]
    async fn a_service_serves_its_routes_once_the_platform_has_connected() {
        let served = serve_probe(true).await;

        // Before the platform attaches, the plugin has not started.
        assert_eq!(get_path(served.address, "/ping", Some(SECRET)).await.0, 404);
        assert!(!served.started.load(Ordering::SeqCst));

        let refused = open_channel(served.address, None)
            .await
            .expect_err("the channel needs the secret");
        assert_eq!(handshake_status(refused), 401);
        assert!(!served.started.load(Ordering::SeqCst));

        let _channel = open_channel(served.address, Some(SECRET)).await.unwrap();
        wait_until_serving(served.address).await;
        assert!(served.started.load(Ordering::SeqCst));
        assert_eq!(
            get_path(served.address, "/ping", Some(SECRET)).await,
            (200, "pong".to_string())
        );
        // The service port is reachable by more than the platform, so the
        // plugin's own routes stay closed without the secret.
        assert_eq!(get_path(served.address, "/ping", None).await.0, 401);
        // The platform can still read the manifest after the plugin started.
        assert_eq!(
            get_path(served.address, PLUGIN_HELLO_PATH, Some(SECRET))
                .await
                .0,
            200
        );
    }

    /// The platform restarts while the plugin's container keeps running: the
    /// plugin accepts the new channel and its calls go to it.
    #[tokio::test]
    async fn a_service_accepts_a_new_channel_when_the_platform_reconnects() {
        let served = serve_probe(true).await;
        let mut first = open_channel(served.address, Some(SECRET)).await.unwrap();
        wait_until_serving(served.address).await;

        let mut second = open_channel(served.address, Some(SECRET))
            .await
            .expect("a service accepts the platform again");

        // The replaced connection is closed by the plugin.
        let closed = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                match first.next().await {
                    None | Some(Ok(Message::Close(_))) | Some(Err(_)) => return,
                    Some(Ok(_)) => continue,
                }
            }
        })
        .await;
        assert!(closed.is_ok(), "the first channel is closed once replaced");

        // A platform call made by the plugin arrives on the new connection.
        let address = served.address;
        let request =
            tokio::spawn(async move { get_path(address, "/projects", Some(SECRET)).await });
        let answered = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let Some(Ok(Message::Text(text))) = second.next().await else {
                    panic!("the plugin's call never reached the new channel");
                };
                if let Ok(ChannelMessage::Request(call)) = serde_json::from_str(&text) {
                    let response = ChannelMessage::Response(ChannelResponse::ok(
                        call.id,
                        PlatformCallResponse::ListProjects(Vec::new()),
                    ));
                    second
                        .send(Message::Text(
                            serde_json::to_string(&response).unwrap().into(),
                        ))
                        .await
                        .unwrap();
                    return;
                }
            }
        })
        .await;
        assert!(
            answered.is_ok(),
            "the new channel carries the plugin's calls"
        );
        assert_eq!(request.await.unwrap(), (200, "0".to_string()));
    }

    /// With no platform connected, a call fails at once instead of waiting
    /// for a connection that may never come.
    #[tokio::test]
    async fn a_service_whose_platform_went_away_fails_calls_until_it_returns() {
        let served = serve_probe(true).await;
        let channel = open_channel(served.address, Some(SECRET)).await.unwrap();
        wait_until_serving(served.address).await;
        drop(channel);

        let mut body = String::new();
        for _ in 0..200 {
            body = get_path(served.address, "/projects", Some(SECRET)).await.1;
            if body.starts_with("error:") {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(body, "error: Platform channel closed unexpectedly");
    }

    /// A plugin started as a child process keeps its single channel: a second
    /// one is refused, and it never answers the service hello.
    #[tokio::test]
    async fn a_child_process_accepts_one_channel_and_no_hello() {
        let served = serve_probe(false).await;
        assert_eq!(
            get_path(served.address, PLUGIN_HELLO_PATH, Some(SECRET))
                .await
                .0,
            404
        );
        let _channel = open_channel(served.address, Some(SECRET)).await.unwrap();
        wait_until_serving(served.address).await;

        // Once started, a child process no longer serves the channel path.
        let refused = open_channel(served.address, Some(SECRET))
            .await
            .expect_err("a child process has exactly one channel");
        assert_eq!(handshake_status(refused), 404);
    }
}
