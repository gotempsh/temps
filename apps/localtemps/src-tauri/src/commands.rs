//! Tauri commands for LocalTemps
//!
//! These commands are callable from the frontend via the Tauri IPC system.

use std::collections::VecDeque;
use std::sync::Arc;

use bollard::models::ContainerSummaryStateEnum;
use bollard::query_parameters::ListContainersOptions;
use bollard::Docker;
use chrono::{DateTime, Utc};
use serde::Serialize;
use tauri::State;
use tokio::sync::RwLock;
use tracing::{error, info};

use crate::context::{
    LocalTempsContext, ServiceStatus, DEFAULT_API_PORT, LOCAL_PROJECT_ID, LOCAL_TOKEN,
};

/// Maximum number of activity logs to keep
const MAX_ACTIVITY_LOGS: usize = 100;

/// Activity log entry
#[derive(Debug, Clone, Serialize)]
pub struct ActivityLogEntry {
    pub timestamp: DateTime<Utc>,
    pub level: String,
    pub message: String,
    pub service: Option<String>,
}

/// App state containing the LocalTemps context
pub struct AppState {
    pub context: Arc<RwLock<Option<Arc<LocalTempsContext>>>>,
    pub api_running: Arc<RwLock<bool>>,
    pub activity_logs: Arc<RwLock<VecDeque<ActivityLogEntry>>>,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            context: Arc::new(RwLock::new(None)),
            api_running: Arc::new(RwLock::new(false)),
            activity_logs: Arc::new(RwLock::new(VecDeque::with_capacity(MAX_ACTIVITY_LOGS))),
        }
    }

    /// Add a log entry
    pub async fn add_log(&self, level: &str, message: &str, service: Option<&str>) {
        let mut logs = self.activity_logs.write().await;
        if logs.len() >= MAX_ACTIVITY_LOGS {
            logs.pop_front();
        }
        logs.push_back(ActivityLogEntry {
            timestamp: Utc::now(),
            level: level.to_string(),
            message: message.to_string(),
            service: service.map(|s| s.to_string()),
        });
    }
}

/// Environment configuration for SDK usage
#[derive(Serialize, Clone)]
pub struct EnvConfig {
    pub api_url: String,
    pub token: String,
    pub project_id: i32,
    pub env_vars: String,
}

/// Result wrapper for Tauri commands
#[derive(Serialize)]
pub struct CommandResult<T> {
    pub success: bool,
    pub data: Option<T>,
    pub error: Option<String>,
}

impl<T> CommandResult<T> {
    pub fn ok(data: T) -> Self {
        Self {
            success: true,
            data: Some(data),
            error: None,
        }
    }

    pub fn err(message: &str) -> Self {
        Self {
            success: false,
            data: None,
            error: Some(message.to_string()),
        }
    }
}

/// Get activity logs
#[tauri::command]
pub async fn get_activity_logs(state: State<'_, AppState>) -> Result<Vec<ActivityLogEntry>, ()> {
    let logs = state.activity_logs.read().await;
    Ok(logs.iter().cloned().collect())
}

/// Clear activity logs
#[tauri::command]
pub async fn clear_activity_logs(state: State<'_, AppState>) -> Result<(), ()> {
    let mut logs = state.activity_logs.write().await;
    logs.clear();
    Ok(())
}

/// Initialize and start all services
#[tauri::command]
pub async fn start_services(
    state: State<'_, AppState>,
) -> Result<CommandResult<Vec<ServiceStatus>>, ()> {
    info!("Starting LocalTemps services...");
    state
        .add_log("info", "Starting LocalTemps services...", None)
        .await;

    // Check if already initialized
    {
        let ctx_guard = state.context.read().await;
        if ctx_guard.is_some() {
            let ctx = ctx_guard.as_ref().unwrap();
            if ctx.is_initialized().await {
                let statuses = ctx.get_service_status().await;
                state
                    .add_log("info", "Services already running", None)
                    .await;
                return Ok(CommandResult::ok(statuses));
            }
        }
    }

    // Create new context
    let ctx = match LocalTempsContext::new().await {
        Ok(ctx) => Arc::new(ctx),
        Err(e) => {
            error!("Failed to create LocalTemps context: {}", e);
            state
                .add_log("error", &format!("Failed to create context: {}", e), None)
                .await;
            return Ok(CommandResult::err(&format!(
                "Failed to create context: {}",
                e
            )));
        }
    };

    // Initialize services
    state
        .add_log("info", "Initializing Docker containers...", None)
        .await;
    if let Err(e) = ctx.init_services().await {
        error!("Failed to initialize services: {}", e);
        state
            .add_log(
                "error",
                &format!("Failed to initialize services: {}", e),
                None,
            )
            .await;
        return Ok(CommandResult::err(&format!(
            "Failed to initialize services: {}",
            e
        )));
    }

    // Store context
    {
        let mut ctx_guard = state.context.write().await;
        *ctx_guard = Some(ctx.clone());
    }

    let statuses = ctx.get_service_status().await;
    for status in &statuses {
        state
            .add_log(
                "success",
                &format!("{} started", status.name),
                Some(&status.service_type),
            )
            .await;
    }
    info!("Services started successfully");
    state
        .add_log("success", "All services started successfully", None)
        .await;
    Ok(CommandResult::ok(statuses))
}

/// Stop all services
#[tauri::command]
pub async fn stop_services(state: State<'_, AppState>) -> Result<CommandResult<()>, ()> {
    info!("Stopping LocalTemps services...");
    state
        .add_log("info", "Stopping LocalTemps services...", None)
        .await;

    let ctx_guard = state.context.read().await;
    if let Some(ctx) = ctx_guard.as_ref() {
        if let Err(e) = ctx.stop_services().await {
            error!("Failed to stop services: {}", e);
            state
                .add_log("error", &format!("Failed to stop services: {}", e), None)
                .await;
            return Ok(CommandResult::err(&format!(
                "Failed to stop services: {}",
                e
            )));
        }
    }

    info!("Services stopped successfully");
    state.add_log("success", "All services stopped", None).await;
    Ok(CommandResult::ok(()))
}

/// Check Docker directly for running localtemps containers
/// This is used when no context exists yet (e.g., at startup)
async fn check_docker_containers() -> Vec<ServiceStatus> {
    // Try to connect to Docker
    let docker = match Docker::connect_with_local_defaults() {
        Ok(d) => d,
        Err(e) => {
            error!("Failed to connect to Docker: {}", e);
            // Return default stopped status if we can't connect to Docker
            return vec![
                ServiceStatus {
                    name: "Redis (KV)".to_string(),
                    service_type: "kv".to_string(),
                    running: false,
                    port: None,
                    connection_info: None,
                },
                ServiceStatus {
                    name: "RustFS (Blob)".to_string(),
                    service_type: "blob".to_string(),
                    running: false,
                    port: None,
                    connection_info: None,
                },
            ];
        }
    };

    // List all containers (including stopped ones)
    let containers = match docker
        .list_containers(Some(ListContainersOptions {
            all: true,
            ..Default::default()
        }))
        .await
    {
        Ok(c) => c,
        Err(e) => {
            error!("Failed to list Docker containers: {}", e);
            return vec![
                ServiceStatus {
                    name: "Redis (KV)".to_string(),
                    service_type: "kv".to_string(),
                    running: false,
                    port: None,
                    connection_info: None,
                },
                ServiceStatus {
                    name: "RustFS (Blob)".to_string(),
                    service_type: "blob".to_string(),
                    running: false,
                    port: None,
                    connection_info: None,
                },
            ];
        }
    };

    // Check for Redis container
    let mut redis_running = false;
    let mut redis_port: Option<u16> = None;
    let mut redis_connection_info: Option<String> = None;

    // Check for RustFS container
    let mut rustfs_running = false;
    let mut rustfs_port: Option<u16> = None;
    let mut rustfs_connection_info: Option<String> = None;

    for container in containers {
        if let Some(names) = &container.names {
            // Check Redis container
            if names.iter().any(|n| n.contains("localtemps-redis")) {
                redis_running = container.state == Some(ContainerSummaryStateEnum::RUNNING);
                if redis_running {
                    if let Some(ports) = &container.ports {
                        for port in ports {
                            if port.private_port == 6379 {
                                if let Some(public_port) = port.public_port {
                                    redis_port = Some(public_port);
                                    redis_connection_info =
                                        Some(format!("redis://localhost:{}", public_port));
                                }
                            }
                        }
                    }
                }
            }

            // Check RustFS container
            if names.iter().any(|n| n.contains("localtemps-rustfs")) {
                rustfs_running = container.state == Some(ContainerSummaryStateEnum::RUNNING);
                if rustfs_running {
                    if let Some(ports) = &container.ports {
                        for port in ports {
                            if port.private_port == 9000 {
                                if let Some(public_port) = port.public_port {
                                    rustfs_port = Some(public_port);
                                    rustfs_connection_info =
                                        Some(format!("http://localhost:{}", public_port));
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    vec![
        ServiceStatus {
            name: "Redis (KV)".to_string(),
            service_type: "kv".to_string(),
            running: redis_running,
            port: redis_port,
            connection_info: redis_connection_info,
        },
        ServiceStatus {
            name: "RustFS (Blob)".to_string(),
            service_type: "blob".to_string(),
            running: rustfs_running,
            port: rustfs_port,
            connection_info: rustfs_connection_info,
        },
    ]
}

/// Get status of all services
#[tauri::command]
pub async fn get_services_status(
    state: State<'_, AppState>,
) -> Result<CommandResult<Vec<ServiceStatus>>, ()> {
    let ctx_guard = state.context.read().await;
    if let Some(ctx) = ctx_guard.as_ref() {
        let statuses = ctx.get_service_status().await;
        Ok(CommandResult::ok(statuses))
    } else {
        // No context yet - check Docker directly for running containers
        let statuses = check_docker_containers().await;
        Ok(CommandResult::ok(statuses))
    }
}

/// Get environment configuration for SDK usage
#[tauri::command]
pub async fn get_env_config() -> Result<EnvConfig, ()> {
    let api_url = format!("http://localhost:{}", DEFAULT_API_PORT);
    let env_vars = format!(
        "TEMPS_API_URL={}\nTEMPS_TOKEN={}\nTEMPS_PROJECT_ID={}",
        api_url, LOCAL_TOKEN, LOCAL_PROJECT_ID
    );

    Ok(EnvConfig {
        api_url,
        token: LOCAL_TOKEN.to_string(),
        project_id: LOCAL_PROJECT_ID,
        env_vars,
    })
}

/// Check if API server is running
#[tauri::command]
pub async fn is_api_running(state: State<'_, AppState>) -> Result<bool, ()> {
    let running = state.api_running.read().await;
    Ok(*running)
}

/// Start the API server
#[tauri::command]
pub async fn start_api_server(state: State<'_, AppState>) -> Result<CommandResult<String>, ()> {
    info!("Starting API server...");
    state.add_log("info", "Starting API server...", None).await;

    // Check if already running
    {
        let running = state.api_running.read().await;
        if *running {
            state
                .add_log("info", "API server already running", None)
                .await;
            return Ok(CommandResult::ok(format!(
                "http://localhost:{}",
                DEFAULT_API_PORT
            )));
        }
    }

    // Get context
    let ctx = {
        let ctx_guard = state.context.read().await;
        match ctx_guard.as_ref() {
            Some(ctx) => ctx.clone(),
            None => {
                state
                    .add_log(
                        "error",
                        "Services not initialized. Start services first.",
                        None,
                    )
                    .await;
                return Ok(CommandResult::err(
                    "Services not initialized. Start services first.",
                ));
            }
        }
    };

    // Mark as running
    {
        let mut running = state.api_running.write().await;
        *running = true;
    }

    // Start API server in background
    let api_running = state.api_running.clone();
    tokio::spawn(async move {
        if let Err(e) = crate::api::create_api_server(ctx, None, DEFAULT_API_PORT).await {
            error!("API server error: {}", e);
        }
        // Mark as not running when server stops
        let mut running = api_running.write().await;
        *running = false;
    });

    info!("API server started on port {}", DEFAULT_API_PORT);
    state
        .add_log(
            "success",
            &format!(
                "API server started on http://localhost:{}",
                DEFAULT_API_PORT
            ),
            None,
        )
        .await;
    Ok(CommandResult::ok(format!(
        "http://localhost:{}",
        DEFAULT_API_PORT
    )))
}
