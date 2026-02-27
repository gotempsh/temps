//! Plugin binary runtime — handles startup, handshake, and serving.

use std::path::PathBuf;
use std::sync::Arc;

use axum::routing::get;
use axum::Router;
use clap::Parser;
use hyper_util::rt::TokioIo;
use sea_orm::Database;
use tokio::net::UnixListener;
use tower::Service;
use tracing::{error, info};

use crate::context::PluginContext;
use crate::manifest::{HandshakeMessage, PluginReady};
use crate::protocol::PluginArgs;
use crate::ExternalPlugin;

/// Run an external plugin. Called by the `main!` macro.
///
/// This function:
/// 1. Parses CLI args
/// 2. Sets up tracing
/// 3. Connects to the database
/// 4. Calls `plugin.manifest()` and writes it to stdout
/// 5. Builds the router from `plugin.router(ctx)`
/// 6. Starts axum on the Unix socket
/// 7. Writes the ready signal to stdout
/// 8. Serves until SIGTERM
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

    // Step 2: Connect to database if required
    let db = if manifest.requires_db {
        info!(plugin = %plugin_name, "Connecting to database");
        let conn = Database::connect(&args.database_url).await.map_err(|e| {
            crate::error::PluginSdkError::DatabaseConnection {
                plugin_name: plugin_name.clone(),
                reason: e.to_string(),
            }
        })?;
        info!(plugin = %plugin_name, "Database connected");
        Arc::new(conn)
    } else {
        let conn = Database::connect("sqlite::memory:").await.map_err(|e| {
            crate::error::PluginSdkError::DatabaseConnection {
                plugin_name: plugin_name.clone(),
                reason: format!("Failed to create placeholder DB: {}", e),
            }
        })?;
        Arc::new(conn)
    };

    // Step 3: Ensure data directory exists
    let data_dir = PathBuf::from(&args.data_dir);
    tokio::fs::create_dir_all(&data_dir).await.map_err(|e| {
        crate::error::PluginSdkError::Initialization {
            plugin_name: plugin_name.clone(),
            reason: format!("Failed to create data dir {}: {}", data_dir.display(), e),
        }
    })?;

    // Step 4: Build plugin context
    let ctx = PluginContext::new(db, plugin_name.clone(), data_dir, args.auth_secret.clone());

    // Step 5: Call on_start hook
    plugin.on_start(&ctx)?;

    // Step 6: Build the router
    let plugin_router = plugin.router(ctx);

    // Wrap with health endpoint
    let app = Router::new()
        .route(&manifest.health_path, get(health_handler))
        .merge(plugin_router);

    // Step 7: Remove stale socket file if it exists
    let socket_path = PathBuf::from(&args.socket_path);
    if socket_path.exists() {
        tokio::fs::remove_file(&socket_path).await.map_err(|e| {
            crate::error::PluginSdkError::SocketBind {
                path: args.socket_path.clone(),
                reason: format!("Failed to remove stale socket: {}", e),
            }
        })?;
    }

    // Step 8: Bind Unix socket
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

    // Step 9: Signal ready to Temps (handshake phase 2)
    let ready_msg = HandshakeMessage::Ready(PluginReady {
        ready: true,
        has_ui: plugin.ui_assets().is_some(),
    });
    let ready_json = serde_json::to_string(&ready_msg)?;
    println!("{}", ready_json);

    // Step 10: Serve requests using hyper on the Unix socket
    let shutdown = tokio::signal::ctrl_c();
    tokio::pin!(shutdown);

    loop {
        tokio::select! {
            accept = listener.accept() => {
                match accept {
                    Ok((stream, _addr)) => {
                        let tower_service = app.clone();
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
                            .serve_connection(socket, hyper_service)
                            .await
                            {
                                // Ignore "error shutting down connection" — this is a normal
                                // HTTP/1.1 keep-alive cleanup when the client closes first
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
            _ = &mut shutdown => {
                info!(plugin = %plugin_name, "Received shutdown signal");
                plugin.on_shutdown();
                break;
            }
        }
    }

    // Cleanup socket
    let _ = tokio::fs::remove_file(&socket_path).await;
    info!(plugin = %plugin_name, "Plugin shut down cleanly");

    Ok(())
}

/// Simple health check handler.
async fn health_handler() -> &'static str {
    "ok"
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
