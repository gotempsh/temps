//! HTTP handlers for the agent API.
//!
//! These wrap the local `ContainerDeployer` and `ImageBuilder` traits,
//! exposing them over HTTP for remote control from the control plane.

use axum::{
    body::Body,
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, Query, State,
    },
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use bollard::exec::StartExecResults;
use bollard::query_parameters::LogsOptions;
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_deployer::{ContainerDeployer, DeployRequest, ImageBuilder};
use tokio::io::AsyncWriteExt;
use utoipa::{OpenApi, ToSchema};

use crate::NodeHealthReport;

/// Shared state for all agent handlers.
pub struct AgentState {
    pub container_deployer: Arc<dyn ContainerDeployer>,
    pub image_builder: Arc<dyn ImageBuilder>,
    /// Direct Docker client for service operations (create/exec/backup).
    /// None if Docker is not available (shouldn't happen on a real agent).
    pub docker: Option<bollard::Docker>,
    /// Bridge gateway IP for the multi-host overlay (`br-temps0`). The
    /// per-node Hickory DNS resolver listens on this address:53; we
    /// inject it as `--dns` into every container we create so they can
    /// resolve `*.temps.local` natively.
    ///
    /// Populated by `network_sync` once the overlay is bootstrapped.
    /// `None` on single-host setups (the overlay never came up); the
    /// container-create path falls back to Docker's default DNS.
    pub overlay_bridge_address: Arc<std::sync::RwLock<Option<std::net::IpAddr>>>,
    /// Latest peer list from the control plane, refreshed by
    /// `network_sync`. Read by overlay-attach handlers to install
    /// per-peer routes inside each new container's netns. Empty until
    /// the first successful network/peers poll.
    pub overlay_peers: crate::network_sync::SharedPeers,
}

/// Response wrapper for consistent agent API responses.
#[derive(Serialize, ToSchema)]
pub struct AgentResponse<T: Serialize> {
    pub(crate) success: bool,
    #[schema(nullable = true)]
    pub(crate) data: Option<T>,
    #[schema(nullable = true)]
    pub(crate) error: Option<String>,
}

impl<T: Serialize> AgentResponse<T> {
    pub(crate) fn ok(data: T) -> Json<Self> {
        Json(Self {
            success: true,
            data: Some(data),
            error: None,
        })
    }
}

fn error_response(status: StatusCode, message: String) -> impl IntoResponse {
    (
        status,
        Json(AgentResponse::<()> {
            success: false,
            data: None,
            error: Some(message),
        }),
    )
}

#[derive(OpenApi)]
#[openapi(
    paths(
        deploy_container,
        stop_container,
        start_container,
        remove_container,
        get_container_logs,
        exec_container,
        get_container_info,
        list_containers,
        image_exists,
        import_image,
        health_check,
        crate::service_handlers::create_service,
        crate::service_handlers::stop_service,
        crate::service_handlers::start_service,
        crate::service_handlers::remove_service,
        crate::service_handlers::service_status,
        crate::service_handlers::service_exec,
        crate::service_handlers::list_services,
        crate::service_handlers::backup_service,
        crate::service_handlers::restore_service,
    ),
    components(schemas(
        AgentResponse<temps_deployer::DeployResult>,
        AgentResponse<String>,
        AgentResponse<bool>,
        AgentResponse<temps_deployer::ContainerInfo>,
        AgentResponse<NodeHealthReport>,
        AgentResponse<crate::ServiceCreateResponse>,
        AgentResponse<crate::ServiceExecResponse>,
        AgentResponse<crate::ServiceStatus>,
        AgentResponse<Vec<crate::ServiceStatus>>,
        AgentResponse<crate::ServiceBackupResponse>,
        AgentResponse<AgentExecResponse>,
        AgentExecRequest,
        AgentExecResponse,
        NodeHealthReport,
        temps_deployer::DeployRequest,
        temps_deployer::DeployResult,
        temps_deployer::ContainerInfo,
        temps_deployer::ContainerStatus,
        temps_deployer::PortMapping,
        temps_deployer::Protocol,
        temps_deployer::ResourceLimits,
        temps_deployer::RestartPolicy,
        temps_deployer::ContainerLogConfig,
        crate::ServiceCreateRequest,
        crate::ServiceCreateResponse,
        crate::ServicePortMapping,
        crate::ServiceExecRequest,
        crate::ServiceExecResponse,
        crate::ServiceBackupRequest,
        crate::ServiceBackupResponse,
        crate::ServiceRestoreRequest,
        crate::S3CredentialsPayload,
        crate::ServiceStatus,
    )),
    info(
        title = "Temps Agent API",
        description = "Worker node agent API for container and service management. All endpoints require Bearer token authentication.",
        version = "1.0.0"
    ),
    security(
        ("bearer_auth" = [])
    ),
    modifiers(&SecurityAddon)
)]
pub struct AgentApiDoc;

struct SecurityAddon;

impl utoipa::Modify for SecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        if let Some(components) = openapi.components.as_mut() {
            components.add_security_scheme(
                "bearer_auth",
                utoipa::openapi::security::SecurityScheme::Http(
                    utoipa::openapi::security::Http::new(
                        utoipa::openapi::security::HttpAuthScheme::Bearer,
                    ),
                ),
            );
        }
    }
}

/// Deploy a new container on this worker node
#[utoipa::path(
    tag = "Containers",
    post,
    path = "/agent/containers/deploy",
    request_body = DeployRequest,
    responses(
        (status = 200, description = "Container deployed successfully", body = AgentResponse<temps_deployer::DeployResult>),
        (status = 401, description = "Unauthorized — invalid or missing bearer token"),
        (status = 500, description = "Deploy failed")
    ),
    security(("bearer_auth" = []))
)]
pub async fn deploy_container(
    State(state): State<Arc<AgentState>>,
    Json(request): Json<DeployRequest>,
) -> impl IntoResponse {
    let container_name = request.container_name.clone();
    let image_name = request.image_name.clone();
    tracing::info!(
        container = %container_name,
        image = %image_name,
        ports = ?request.port_mappings.iter().map(|p| format!("{}:{}", p.host_port, p.container_port)).collect::<Vec<_>>(),
        "Deploying container"
    );
    match state.container_deployer.deploy_container(request).await {
        Ok(result) => {
            tracing::info!(
                container = %container_name,
                container_id = %result.container_id,
                image = %image_name,
                "Container deployed successfully"
            );
            AgentResponse::ok(result).into_response()
        }
        Err(e) => {
            tracing::error!(
                container = %container_name,
                image = %image_name,
                "Deploy failed: {}",
                e
            );
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Deploy failed: {}", e),
            )
            .into_response()
        }
    }
}

/// Stop a running container
#[utoipa::path(
    tag = "Containers",
    post,
    path = "/agent/containers/{id}/stop",
    params(
        ("id" = String, Path, description = "Container ID or name")
    ),
    responses(
        (status = 200, description = "Container stopped", body = AgentResponse<String>),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Stop failed")
    ),
    security(("bearer_auth" = []))
)]
pub async fn stop_container(
    State(state): State<Arc<AgentState>>,
    Path(container_id): Path<String>,
) -> impl IntoResponse {
    tracing::info!(container_id = %container_id, "Stopping container");
    match state.container_deployer.stop_container(&container_id).await {
        Ok(()) => {
            tracing::info!(container_id = %container_id, "Container stopped");
            AgentResponse::ok("stopped".to_string()).into_response()
        }
        Err(e) => {
            tracing::error!(container_id = %container_id, "Stop failed: {}", e);
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Stop failed for container {}: {}", container_id, e),
            )
            .into_response()
        }
    }
}

/// Start a stopped container.
///
/// Used by the control plane when the user clicks Start on a container
/// running on this worker. Returns the same `AgentResponse<String>`
/// envelope as `stop_container` so `RemoteNodeDeployer` can decode it
/// uniformly.
#[utoipa::path(
    tag = "Containers",
    post,
    path = "/agent/containers/{id}/start",
    params(
        ("id" = String, Path, description = "Container ID or name")
    ),
    responses(
        (status = 200, description = "Container started", body = AgentResponse<String>),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Start failed")
    ),
    security(("bearer_auth" = []))
)]
pub async fn start_container(
    State(state): State<Arc<AgentState>>,
    Path(container_id): Path<String>,
) -> impl IntoResponse {
    tracing::info!(container_id = %container_id, "Starting container");
    match state
        .container_deployer
        .start_container(&container_id)
        .await
    {
        Ok(()) => {
            tracing::info!(container_id = %container_id, "Container started");
            AgentResponse::ok("started".to_string()).into_response()
        }
        Err(e) => {
            tracing::error!(container_id = %container_id, "Start failed: {}", e);
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Start failed for container {}: {}", container_id, e),
            )
            .into_response()
        }
    }
}

/// Remove a container
#[utoipa::path(
    tag = "Containers",
    delete,
    path = "/agent/containers/{id}",
    params(
        ("id" = String, Path, description = "Container ID or name")
    ),
    responses(
        (status = 200, description = "Container removed", body = AgentResponse<String>),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Remove failed")
    ),
    security(("bearer_auth" = []))
)]
pub async fn remove_container(
    State(state): State<Arc<AgentState>>,
    Path(container_id): Path<String>,
) -> impl IntoResponse {
    tracing::info!(container_id = %container_id, "Removing container");
    match state
        .container_deployer
        .remove_container(&container_id)
        .await
    {
        Ok(()) => {
            tracing::info!(container_id = %container_id, "Container removed");
            AgentResponse::ok("removed".to_string()).into_response()
        }
        Err(e) => {
            tracing::error!(container_id = %container_id, "Remove failed: {}", e);
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Remove failed for container {}: {}", container_id, e),
            )
            .into_response()
        }
    }
}

/// Get container logs
#[utoipa::path(
    tag = "Containers",
    get,
    path = "/agent/containers/{id}/logs",
    params(
        ("id" = String, Path, description = "Container ID or name")
    ),
    responses(
        (status = 200, description = "Container logs", body = AgentResponse<String>),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Failed to get logs")
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_container_logs(
    State(state): State<Arc<AgentState>>,
    Path(container_id): Path<String>,
) -> impl IntoResponse {
    tracing::debug!(container_id = %container_id, "Fetching container logs");
    match state
        .container_deployer
        .get_container_logs(&container_id)
        .await
    {
        Ok(logs) => AgentResponse::ok(logs).into_response(),
        Err(e) => {
            tracing::error!(container_id = %container_id, "Failed to get logs: {}", e);
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to get logs for container {}: {}", container_id, e),
            )
            .into_response()
        }
    }
}

/// One-shot container stats (CPU%, memory, network counters).
///
/// The control plane calls this when the user opens the metrics tab for a
/// container that runs on this node, and on every poll of the SSE stream
/// (the agent itself doesn't stream — the CP polls at its own interval).
///
/// Not registered in the agent OpenAPI doc because `ContainerStats` does
/// not derive `ToSchema` and is only ever read by the control plane via
/// `RemoteNodeDeployer`.
pub async fn get_container_stats(
    State(state): State<Arc<AgentState>>,
    Path(container_id): Path<String>,
) -> impl IntoResponse {
    tracing::debug!(container_id = %container_id, "Fetching container stats");
    match state
        .container_deployer
        .get_container_stats(&container_id)
        .await
    {
        Ok(stats) => AgentResponse::ok(stats).into_response(),
        Err(e) => {
            tracing::error!(container_id = %container_id, "Failed to get stats: {}", e);
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to get stats for container {}: {}", container_id, e),
            )
            .into_response()
        }
    }
}

/// One-shot exec request from the control plane.
///
/// Wire-compatible with the CP's existing `ExecRequest` struct so the
/// remote-deployer client can serialize a single shape regardless of
/// where the container runs.
#[derive(Debug, Deserialize, ToSchema)]
pub struct AgentExecRequest {
    pub command: Vec<String>,
    pub timeout_seconds: Option<u64>,
}

/// Result of a one-shot exec. Mirrors the CP's `ExecResponse`.
#[derive(Debug, Serialize, ToSchema)]
pub struct AgentExecResponse {
    pub exit_code: Option<i64>,
    pub stdout: String,
    pub stderr: String,
}

/// Run a one-shot command inside a container on this worker.
///
/// Container exec is timeout-bounded (default 30s, max 300s) so a hung
/// process can't pin an agent worker thread forever. Output is captured
/// to memory; the caller gets a single JSON response, not a stream — for
/// interactive sessions use the (separate) terminal WebSocket.
#[utoipa::path(
    tag = "Containers",
    post,
    path = "/agent/containers/{id}/exec",
    params(
        ("id" = String, Path, description = "Container ID or name")
    ),
    request_body = AgentExecRequest,
    responses(
        (status = 200, description = "Exec result", body = AgentResponse<AgentExecResponse>),
        (status = 400, description = "Invalid command"),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Exec failed"),
        (status = 504, description = "Exec timed out")
    ),
    security(("bearer_auth" = []))
)]
pub async fn exec_container(
    State(state): State<Arc<AgentState>>,
    Path(container_id): Path<String>,
    Json(request): Json<AgentExecRequest>,
) -> impl IntoResponse {
    if request.command.is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "Command cannot be empty".into())
            .into_response();
    }

    let Some(docker) = state.docker.clone() else {
        return error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "Docker is not available on this agent".into(),
        )
        .into_response();
    };

    let timeout_secs = std::cmp::min(request.timeout_seconds.unwrap_or(30), 300);

    tracing::info!(
        container_id = %container_id,
        timeout_secs,
        cmd_argc = request.command.len(),
        "Executing one-shot command"
    );

    let exec_config = bollard::models::ExecConfig {
        attach_stdout: Some(true),
        attach_stderr: Some(true),
        cmd: Some(request.command.clone()),
        ..Default::default()
    };

    let exec = match docker.create_exec(&container_id, exec_config).await {
        Ok(e) => e,
        Err(bollard::errors::Error::DockerResponseServerError {
            status_code: 404, ..
        }) => {
            return error_response(
                StatusCode::NOT_FOUND,
                format!("Container {} not found", container_id),
            )
            .into_response();
        }
        Err(e) => {
            tracing::error!(container_id = %container_id, "Failed to create exec: {}", e);
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to create exec: {}", e),
            )
            .into_response();
        }
    };

    let start_config = bollard::exec::StartExecOptions {
        detach: false,
        ..Default::default()
    };

    let result = tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), async {
        let output = docker.start_exec(&exec.id, Some(start_config)).await?;
        let mut stdout = String::new();
        let mut stderr = String::new();
        if let bollard::exec::StartExecResults::Attached { mut output, .. } = output {
            while let Some(Ok(msg)) = output.next().await {
                match msg {
                    bollard::container::LogOutput::StdOut { message } => {
                        stdout.push_str(&String::from_utf8_lossy(&message));
                    }
                    bollard::container::LogOutput::StdErr { message } => {
                        stderr.push_str(&String::from_utf8_lossy(&message));
                    }
                    _ => {}
                }
            }
        }
        Ok::<_, bollard::errors::Error>((stdout, stderr))
    })
    .await;

    match result {
        Ok(Ok((stdout, stderr))) => {
            let exit_code = docker
                .inspect_exec(&exec.id)
                .await
                .ok()
                .and_then(|i| i.exit_code);
            tracing::info!(
                container_id = %container_id,
                exit_code = ?exit_code,
                "Exec completed"
            );
            AgentResponse::ok(AgentExecResponse {
                exit_code,
                stdout,
                stderr,
            })
            .into_response()
        }
        Ok(Err(e)) => {
            tracing::error!(container_id = %container_id, "Exec error: {}", e);
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Exec error: {}", e),
            )
            .into_response()
        }
        Err(_) => {
            tracing::warn!(container_id = %container_id, timeout_secs, "Exec timed out");
            error_response(
                StatusCode::GATEWAY_TIMEOUT,
                format!("Command timed out after {}s", timeout_secs),
            )
            .into_response()
        }
    }
}

/// Persistent terminal session via WebSocket on the worker.
///
/// Speaks the same protocol the browser-facing CP terminal speaks:
///   - client binary frames -> container PTY stdin
///   - container PTY output -> server binary frames (xterm.js renders these)
///   - client text frame `{"type":"resize","cols":N,"rows":N}` -> resize PTY
///   - client text frame `{"type":"input","data":"..."}` -> stdin (legacy)
///   - server text frame `{"type":"exit","code":N}` when exec ends
///
/// The control plane's terminal handler proxies bytes 1:1 between the
/// browser WS and this WS, so exactly the same xterm.js client works
/// against a remote container. No protocol translation in the middle.
pub async fn terminal_container(
    State(state): State<Arc<AgentState>>,
    Path(container_id): Path<String>,
    ws: WebSocketUpgrade,
) -> Response {
    let Some(docker) = state.docker.clone() else {
        return error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "Docker is not available on this agent".into(),
        )
        .into_response();
    };

    ws.on_upgrade(move |socket| handle_terminal_session(socket, docker, container_id))
        .into_response()
}

#[derive(Deserialize)]
struct TerminalControl {
    r#type: String,
    cols: Option<u16>,
    rows: Option<u16>,
    data: Option<String>,
}

async fn handle_terminal_session(socket: WebSocket, docker: bollard::Docker, container_id: String) {
    tracing::debug!(container_id = %container_id, "Agent terminal session started");

    // Try bash, fall back to sh — same shape as the CP-local terminal so
    // remote sessions feel identical.
    let exec_config = bollard::models::ExecConfig {
        attach_stdin: Some(true),
        attach_stdout: Some(true),
        attach_stderr: Some(true),
        tty: Some(true),
        cmd: Some(vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "if command -v bash >/dev/null 2>&1; then exec bash; else exec sh; fi".to_string(),
        ]),
        ..Default::default()
    };

    let exec = match docker.create_exec(&container_id, exec_config).await {
        Ok(e) => e,
        Err(e) => {
            tracing::error!(error = %e, container_id = %container_id, "Failed to create exec for terminal");
            return;
        }
    };
    let exec_id = exec.id.clone();

    let start_config = bollard::exec::StartExecOptions {
        detach: false,
        tty: true,
        ..Default::default()
    };

    let (mut docker_output, mut docker_input) = match docker
        .start_exec(&exec_id, Some(start_config))
        .await
    {
        Ok(StartExecResults::Attached { output, input }) => (output, input),
        Ok(StartExecResults::Detached) => {
            tracing::error!("Exec started in detached mode unexpectedly");
            return;
        }
        Err(e) => {
            tracing::error!(error = %e, container_id = %container_id, "Failed to start exec for terminal");
            return;
        }
    };

    let (mut ws_sender, mut ws_receiver) = socket.split();

    // PTY -> WS
    let exec_id_for_output = exec_id.clone();
    let docker_for_output = docker.clone();
    let output_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = docker_output.next().await {
            let bytes: bytes::Bytes = match msg {
                bollard::container::LogOutput::StdOut { message } => message,
                bollard::container::LogOutput::StdErr { message } => message,
                bollard::container::LogOutput::Console { message } => message,
                _ => continue,
            };
            if ws_sender
                .send(Message::Binary(bytes.to_vec().into()))
                .await
                .is_err()
            {
                break;
            }
        }

        let exit_code = docker_for_output
            .inspect_exec(&exec_id_for_output)
            .await
            .ok()
            .and_then(|i| i.exit_code)
            .unwrap_or(-1);
        let exit_msg = format!(r#"{{"type":"exit","code":{}}}"#, exit_code);
        let _ = ws_sender.send(Message::Text(exit_msg.into())).await;
        let _ = ws_sender.close().await;
    });

    // WS -> PTY
    let idle_timeout = std::time::Duration::from_secs(15 * 60);
    loop {
        let next = tokio::time::timeout(idle_timeout, ws_receiver.next()).await;
        match next {
            Ok(Some(Ok(Message::Binary(data)))) => {
                if docker_input.write_all(&data).await.is_err() {
                    break;
                }
                if docker_input.flush().await.is_err() {
                    break;
                }
            }
            Ok(Some(Ok(Message::Text(text)))) => {
                if let Ok(ctrl) = serde_json::from_str::<TerminalControl>(&text) {
                    match ctrl.r#type.as_str() {
                        "resize" => {
                            if let (Some(cols), Some(rows)) = (ctrl.cols, ctrl.rows) {
                                let resize_opts = bollard::exec::ResizeExecOptions {
                                    width: cols,
                                    height: rows,
                                };
                                if let Err(e) = docker.resize_exec(&exec_id, resize_opts).await {
                                    tracing::warn!(error = %e, "Failed to resize terminal");
                                }
                            }
                        }
                        "input" => {
                            if let Some(data) = ctrl.data {
                                if docker_input.write_all(data.as_bytes()).await.is_err() {
                                    break;
                                }
                                let _ = docker_input.flush().await;
                            }
                        }
                        _ => {}
                    }
                } else if docker_input.write_all(text.as_bytes()).await.is_err() {
                    break;
                }
            }
            Ok(Some(Ok(Message::Close(_)))) | Ok(None) => {
                tracing::debug!(container_id = %container_id, "Agent terminal closed by client");
                break;
            }
            Err(_) => {
                tracing::info!(container_id = %container_id, "Agent terminal idle 15m, closing");
                break;
            }
            _ => {}
        }
    }

    output_task.abort();
    tracing::info!(container_id = %container_id, "Agent terminal session ended");
}

/// Query parameters for the streaming logs endpoint. Mirrors the control
/// plane's `ContainerLogsQuery` so the proxy can pass them through verbatim.
#[derive(Debug, Deserialize)]
pub struct ContainerLogsStreamQuery {
    /// Unix timestamp (seconds). `0` or absent = beginning.
    pub start_date: Option<i64>,
    /// Unix timestamp (seconds). `0` or absent = no upper bound.
    pub end_date: Option<i64>,
    /// `"all"` or a number of trailing lines.
    pub tail: Option<String>,
    /// Prefix every line with the Docker timestamp.
    #[serde(default)]
    pub timestamps: bool,
    /// `true` to stream new lines as they arrive (default), `false` to dump
    /// the existing logs and close.
    #[serde(default = "default_true")]
    pub follow: bool,
}

fn default_true() -> bool {
    true
}

/// Stream container logs over a chunked HTTP body.
///
/// The control plane proxies each chunk to the browser as a WebSocket
/// Text frame, so callers see exactly what they would see if they hit the
/// existing in-process log path on a single-host cluster.
pub async fn stream_container_logs(
    State(state): State<Arc<AgentState>>,
    Path(container_id): Path<String>,
    Query(params): Query<ContainerLogsStreamQuery>,
) -> Response {
    let Some(docker) = state.docker.clone() else {
        return error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "Docker is not available on this agent".into(),
        )
        .into_response();
    };

    // Inspect first so we can return a clean 404 instead of a half-open
    // chunked body that errors mid-stream.
    if let Err(e) = docker
        .inspect_container(
            &container_id,
            None::<bollard::query_parameters::InspectContainerOptions>,
        )
        .await
    {
        return match e {
            bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            } => error_response(
                StatusCode::NOT_FOUND,
                format!("Container {} not found", container_id),
            )
            .into_response(),
            other => error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to inspect container {}: {}", container_id, other),
            )
            .into_response(),
        };
    }

    let log_options = LogsOptions {
        follow: params.follow,
        stdout: true,
        stderr: true,
        timestamps: params.timestamps,
        tail: params.tail.unwrap_or_else(|| "all".into()),
        since: params.start_date.unwrap_or(0) as i32,
        until: params.end_date.unwrap_or(0) as i32,
    };

    tracing::debug!(
        container_id = %container_id,
        follow = params.follow,
        timestamps = params.timestamps,
        "Streaming container logs"
    );

    let logs = docker.logs(&container_id, Some(log_options));
    let log_stream = logs.map(|chunk| match chunk {
        Ok(out) => {
            let bytes: bytes::Bytes = out.into_bytes();
            Ok::<_, std::io::Error>(bytes)
        }
        Err(e) => Err(std::io::Error::other(format!("docker logs error: {}", e))),
    });

    // Interleave a NUL keepalive every 25s when the container is silent so
    // intermediate proxies (Pingora's 60s body read timeout, idle TCP
    // gateways) don't drop the long-lived stream. The control plane filters
    // these out before forwarding to the WebSocket client.
    let keepalive = futures::stream::unfold((), |_| async move {
        tokio::time::sleep(std::time::Duration::from_secs(25)).await;
        Some((
            Ok::<_, std::io::Error>(bytes::Bytes::from_static(b"\0")),
            (),
        ))
    });
    let body_stream = futures::stream::select(log_stream, keepalive);

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        // Disable proxy buffering so log lines flush as they arrive.
        .header("X-Accel-Buffering", "no")
        .header(header::CACHE_CONTROL, "no-cache")
        .body(Body::from_stream(body_stream))
        .unwrap_or_else(|_| {
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to construct log stream response".into(),
            )
            .into_response()
        })
}

/// Get container info (status, ports, environment)
#[utoipa::path(
    tag = "Containers",
    get,
    path = "/agent/containers/{id}/info",
    params(
        ("id" = String, Path, description = "Container ID or name")
    ),
    responses(
        (status = 200, description = "Container info", body = AgentResponse<temps_deployer::ContainerInfo>),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Failed to get info")
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_container_info(
    State(state): State<Arc<AgentState>>,
    Path(container_id): Path<String>,
) -> impl IntoResponse {
    tracing::debug!(container_id = %container_id, "Fetching container info");
    match state
        .container_deployer
        .get_container_info(&container_id)
        .await
    {
        Ok(info) => AgentResponse::ok(info).into_response(),
        Err(e) => {
            tracing::error!(container_id = %container_id, "Failed to get info: {}", e);
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to get info for container {}: {}", container_id, e),
            )
            .into_response()
        }
    }
}

/// List all containers on this worker node
#[utoipa::path(
    tag = "Containers",
    get,
    path = "/agent/containers",
    responses(
        (status = 200, description = "List of containers", body = AgentResponse<Vec<temps_deployer::ContainerInfo>>),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Failed to list containers")
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_containers(State(state): State<Arc<AgentState>>) -> impl IntoResponse {
    tracing::debug!("Listing containers");
    match state.container_deployer.list_containers().await {
        Ok(containers) => AgentResponse::ok(containers).into_response(),
        Err(e) => {
            tracing::error!("Failed to list containers: {}", e);
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to list containers: {}", e),
            )
            .into_response()
        }
    }
}

/// Check if a Docker image exists on this node
#[utoipa::path(
    tag = "Images",
    get,
    path = "/agent/images/{name}/exists",
    params(
        ("name" = String, Path, description = "Docker image name (URL-encoded if it contains slashes)")
    ),
    responses(
        (status = 200, description = "Image existence check result", body = AgentResponse<bool>),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Failed to check image")
    ),
    security(("bearer_auth" = []))
)]
pub async fn image_exists(
    State(state): State<Arc<AgentState>>,
    Path(image_name): Path<String>,
) -> impl IntoResponse {
    tracing::debug!(image = %image_name, "Checking if image exists");
    match state.container_deployer.image_exists(&image_name).await {
        Ok(exists) => {
            tracing::debug!(image = %image_name, exists = exists, "Image existence check complete");
            AgentResponse::ok(exists).into_response()
        }
        Err(e) => {
            tracing::error!(image = %image_name, "Failed to check image: {}", e);
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to check image {}: {}", image_name, e),
            )
            .into_response()
        }
    }
}

/// Import a Docker image from a tar archive streamed in the request body.
///
/// The control plane calls this to transfer locally-built images to worker nodes.
/// The image tag is passed via the `x-image-tag` header.
#[utoipa::path(
    tag = "Images",
    post,
    path = "/agent/images/import",
    request_body(content = Vec<u8>, content_type = "application/x-tar"),
    responses(
        (status = 200, description = "Image imported successfully", body = AgentResponse<String>),
        (status = 400, description = "Missing x-image-tag header"),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Import failed")
    ),
    security(("bearer_auth" = []))
)]
pub async fn import_image(
    State(state): State<Arc<AgentState>>,
    headers: axum::http::HeaderMap,
    body: axum::body::Body,
) -> impl IntoResponse {
    let tag = match headers.get("x-image-tag").and_then(|v| v.to_str().ok()) {
        Some(t) => t.to_string(),
        None => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "Missing required x-image-tag header".to_string(),
            )
            .into_response();
        }
    };

    tracing::info!(image = %tag, "Receiving image tar from control plane");

    // Stream the body to a temp file
    let tmp_dir = std::env::temp_dir();
    let tmp_path = tmp_dir.join(format!("temps-image-import-{}.tar", uuid::Uuid::new_v4()));

    let write_result = async {
        use http_body_util::BodyExt;

        let mut file = tokio::fs::File::create(&tmp_path).await?;
        let mut total_bytes: u64 = 0;

        let mut body = body;
        while let Some(frame) = BodyExt::frame(&mut body).await {
            let frame =
                frame.map_err(|e| std::io::Error::other(format!("Body read error: {}", e)))?;
            if let Ok(data) = frame.into_data() {
                tokio::io::AsyncWriteExt::write_all(&mut file, &data).await?;
                total_bytes += data.len() as u64;
            }
        }
        tokio::io::AsyncWriteExt::flush(&mut file).await?;

        tracing::info!(
            image = %tag,
            size_mb = format!("{:.1}", total_bytes as f64 / 1_048_576.0),
            "Image tar received, loading into Docker"
        );
        Ok::<_, std::io::Error>(())
    }
    .await;

    if let Err(e) = write_result {
        let _ = tokio::fs::remove_file(&tmp_path).await;
        return error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to write image tar: {}", e),
        )
        .into_response();
    }

    // Import the image via the image builder (docker load)
    let result = state
        .image_builder
        .import_image(tmp_path.clone(), &tag)
        .await;

    // Clean up temp file
    let _ = tokio::fs::remove_file(&tmp_path).await;

    match result {
        Ok(image_id) => {
            tracing::info!(image = %tag, image_id = %image_id, "Image imported successfully");
            AgentResponse::ok(image_id).into_response()
        }
        Err(e) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to import image '{}': {}", tag, e),
        )
        .into_response(),
    }
}

/// Health check — returns system metrics for this worker node
#[utoipa::path(
    tag = "Health",
    get,
    path = "/agent/health",
    responses(
        (status = 200, description = "Node health report", body = AgentResponse<NodeHealthReport>),
        (status = 401, description = "Unauthorized")
    ),
    security(("bearer_auth" = []))
)]
pub async fn health_check(State(state): State<Arc<AgentState>>) -> impl IntoResponse {
    let report = collect_system_metrics(&state).await;
    AgentResponse::ok(report)
}

/// Collect real system metrics using sysinfo.
async fn collect_system_metrics(state: &AgentState) -> NodeHealthReport {
    use sysinfo::Disks;

    let mut sys = sysinfo::System::new();
    sys.refresh_cpu_all();
    sys.refresh_memory();
    let disks = Disks::new_with_refreshed_list();

    let cpu_percent = sys.global_cpu_usage() as f64;
    let memory_used_bytes = sys.used_memory();
    let memory_total_bytes = sys.total_memory();

    // Use only the root mount point to avoid double-counting overlapping mounts
    let (disk_used, disk_total) = disks
        .list()
        .iter()
        .find(|d| d.mount_point() == std::path::Path::new("/"))
        .map(|d| (d.total_space() - d.available_space(), d.total_space()))
        .unwrap_or((0, 0));

    // Count running containers via the deployer
    let running_containers = match state.container_deployer.list_containers().await {
        Ok(containers) => containers.len() as u64,
        Err(_) => 0,
    };

    NodeHealthReport {
        cpu_percent,
        memory_used_bytes,
        memory_total_bytes,
        disk_used_bytes: disk_used,
        disk_total_bytes: disk_total,
        running_containers,
    }
}
