//! LocalTemps - Local Development Environment for Temps SDK
//!
//! A Tauri desktop application that provides local KV and Blob services
//! compatible with @temps-sdk/kv and @temps-sdk/blob libraries.
//!
//! # Features
//!
//! - SDK-compatible API endpoints (KV and Blob)
//! - Docker container management (Redis, RustFS)
//! - System tray integration
//! - Service lifecycle management
//!
//! # Usage
//!
//! Start the application and use the SDK with:
//!
//! ```typescript
//! import { createClient } from '@temps-sdk/kv'
//!
//! const kv = createClient({
//!   apiUrl: 'http://localhost:4000',
//!   token: 'localtemps-dev-token',
//!   projectId: 1
//! })
//! ```

pub mod api;
pub mod commands;
pub mod context;
pub mod db;
pub mod entities;
pub mod services;

use std::sync::Arc;

use commands::AppState;
use context::{LocalTempsContext, DEFAULT_API_PORT};
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Manager,
};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

/// Initialize the Tauri application
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Initialize tracing
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer())
        .with(tracing_subscriber::EnvFilter::new(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "info,localtemps=debug".to_string()),
        ))
        .init();

    tracing::info!("Starting LocalTemps...");

    // Create app state
    let app_state = AppState::new();

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_shell::init())
        .manage(app_state)
        .invoke_handler(tauri::generate_handler![
            commands::start_services,
            commands::stop_services,
            commands::get_services_status,
            commands::get_env_config,
            commands::is_api_running,
            commands::start_api_server,
            commands::get_activity_logs,
            commands::clear_activity_logs,
        ])
        .setup(|app| {
            // Create tray menu
            let show_item = MenuItem::with_id(app, "show", "Show Window", true, None::<&str>)?;
            let start_item = MenuItem::with_id(app, "start", "Start Services", true, None::<&str>)?;
            let stop_item = MenuItem::with_id(app, "stop", "Stop Services", true, None::<&str>)?;
            let quit_item = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;

            let menu = Menu::with_items(app, &[&show_item, &start_item, &stop_item, &quit_item])?;

            // Build the tray icon
            let _tray = TrayIconBuilder::new()
                .icon(app.default_window_icon().unwrap().clone())
                .menu(&menu)
                .show_menu_on_left_click(false)
                .tooltip("LocalTemps - Local Development Environment")
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        let app = tray.app_handle();
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                })
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => {
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                    "start" => {
                        let app_handle = app.clone();
                        tauri::async_runtime::spawn(async move {
                            let state = app_handle.state::<AppState>();
                            if let Ok(result) = commands::start_services(state.clone()).await {
                                if result.success {
                                    // Also start API server
                                    let _ = commands::start_api_server(state).await;
                                    tracing::info!("Services started via tray menu");
                                }
                            }
                        });
                    }
                    "stop" => {
                        let app_handle = app.clone();
                        tauri::async_runtime::spawn(async move {
                            let state = app_handle.state::<AppState>();
                            let _ = commands::stop_services(state).await;
                            tracing::info!("Services stopped via tray menu");
                        });
                    }
                    "quit" => {
                        tracing::info!("Quitting LocalTemps...");
                        app.exit(0);
                    }
                    _ => {}
                })
                .build(app)?;

            tracing::info!("System tray initialized");

            // Auto-start API server on app launch
            // Services will auto-initialize on first API request (zero-config)
            let app_handle = app.handle().clone();
            let app_data_dir = app
                .path()
                .app_data_dir()
                .expect("Failed to get app data dir");

            tauri::async_runtime::spawn(async move {
                tracing::info!("Auto-starting API server...");

                // Initialize SQLite database for analytics
                let db = match db::init_database(app_data_dir).await {
                    Ok(db) => {
                        tracing::info!("SQLite database initialized");
                        Some(db)
                    }
                    Err(e) => {
                        tracing::error!("Failed to initialize database: {}", e);
                        let state = app_handle.state::<AppState>();
                        state
                            .add_log(
                                "warning",
                                &format!("Analytics database unavailable: {}", e),
                                None,
                            )
                            .await;
                        None
                    }
                };

                // Create context for the API server
                match LocalTempsContext::new().await {
                    Ok(ctx) => {
                        let ctx = Arc::new(ctx);

                        // Store context in app state
                        let state = app_handle.state::<AppState>();
                        {
                            let mut ctx_guard = state.context.write().await;
                            *ctx_guard = Some(ctx.clone());
                        }

                        // Mark API as running
                        {
                            let mut running = state.api_running.write().await;
                            *running = true;
                        }

                        state
                            .add_log(
                                "info",
                                &format!(
                                    "API server starting on http://localhost:{}",
                                    DEFAULT_API_PORT
                                ),
                                None,
                            )
                            .await;
                        state
                            .add_log("info", "Services will auto-start on first API call", None)
                            .await;

                        // Start the API server with optional analytics
                        if let Err(e) = api::create_api_server(ctx, db, DEFAULT_API_PORT).await {
                            tracing::error!("API server error: {}", e);
                            state
                                .add_log("error", &format!("API server error: {}", e), None)
                                .await;
                        }

                        // Mark as not running when server stops
                        let mut running = state.api_running.write().await;
                        *running = false;
                    }
                    Err(e) => {
                        tracing::error!("Failed to create context: {}", e);
                        let state = app_handle.state::<AppState>();
                        state
                            .add_log(
                                "error",
                                &format!("Failed to start API server: {}. Is Docker running?", e),
                                None,
                            )
                            .await;
                    }
                }
            });

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
