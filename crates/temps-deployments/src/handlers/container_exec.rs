//! Container exec and terminal handlers.
//!
//! Provides one-shot exec (POST) and persistent terminal (WebSocket) access
//! to running containers. Both require the `ContainersExec` permission and
//! the project must have `container_exec_enabled` set to true.

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, State,
    },
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use bollard::exec::StartExecResults;
use bytes::Bytes;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_auth::{permission_guard, RequireAuth};
use temps_core::problemdetails::{self, Problem};
use tokio::io::AsyncWriteExt;
use tracing::{debug, error, info, warn};
use utoipa::ToSchema;

use super::types::AppState;

#[derive(Deserialize, ToSchema)]
pub struct ExecRequest {
    /// Command to execute (e.g. ["sh", "-c", "ls -la"])
    pub command: Vec<String>,
    /// Timeout in seconds (default: 30, max: 300)
    pub timeout_seconds: Option<u64>,
}

#[derive(Serialize, ToSchema)]
pub struct ExecResponse {
    pub exit_code: Option<i64>,
    pub stdout: String,
    pub stderr: String,
}

/// One-shot command execution in a container
#[utoipa::path(
    tag = "Containers",
    post,
    path = "/projects/{project_id}/environments/{environment_id}/containers/{container_id}/exec",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("environment_id" = i32, Path, description = "Environment ID"),
        ("container_id" = String, Path, description = "Container ID"),
    ),
    request_body = ExecRequest,
    responses(
        (status = 200, description = "Command executed", body = ExecResponse),
        (status = 400, description = "Exec not enabled for this project"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Container not found"),
        (status = 500, description = "Exec failed")
    ),
    security(("bearer_auth" = []))
)]
pub async fn exec_command(
    State(state): State<Arc<AppState>>,
    Path((project_id, environment_id, container_id)): Path<(i32, i32, String)>,
    RequireAuth(auth): RequireAuth,
    Json(request): Json<ExecRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ContainersExec);

    if request.command.is_empty() {
        return Err(problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Invalid Command")
            .with_detail("Command cannot be empty"));
    }

    // Verify the container belongs to this project/environment
    let (container_record, _env) = state
        .deployment_service
        .get_container_detail(project_id, environment_id, container_id.clone())
        .await
        .map_err(|_| {
            problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Container Not Found")
                .with_detail(format!(
                    "Container {} not found in project {} environment {}",
                    container_id, project_id, environment_id
                ))
        })?;

    // Use the verified container ID from the database record
    let verified_container_id = &container_record.container_id;

    let timeout = std::cmp::min(request.timeout_seconds.unwrap_or(30), 300);

    // Route to the worker that owns this container. Local containers
    // (`node_id IS NULL`) run on the CP's own dockerd — keep the inline
    // bollard path. Remote containers go through the agent's
    // `/agent/containers/{id}/exec` endpoint, which runs identical bollard
    // logic on the worker.
    if let Some(node_id) = container_record.node_id {
        let result = state
            .deployment_service
            .exec_command_remote(
                node_id,
                verified_container_id,
                request.command.clone(),
                Some(timeout),
            )
            .await
            .map_err(|e| {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Exec Failed")
                    .with_detail(e.to_string())
            })?;

        info!(
            container_id = %container_id,
            node_id,
            exit_code = ?result.exit_code,
            "Remote container exec completed"
        );

        return Ok(Json(ExecResponse {
            exit_code: result.exit_code,
            stdout: result.stdout,
            stderr: result.stderr,
        }));
    }

    let docker = &state.docker;

    // Create exec instance
    let exec_config = bollard::models::ExecConfig {
        attach_stdout: Some(true),
        attach_stderr: Some(true),
        cmd: Some(request.command.clone()),
        ..Default::default()
    };

    let exec = docker
        .create_exec(verified_container_id, exec_config)
        .await
        .map_err(|e| {
            problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Exec Failed")
                .with_detail(format!("Failed to create exec: {}", e))
        })?;

    // Start exec and collect output
    let start_config = bollard::exec::StartExecOptions {
        detach: false,
        ..Default::default()
    };

    let result = tokio::time::timeout(std::time::Duration::from_secs(timeout), async {
        let output = docker.start_exec(&exec.id, Some(start_config)).await?;

        let mut stdout = String::new();
        let mut stderr = String::new();

        if let StartExecResults::Attached { mut output, .. } = output {
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
            let inspect = docker.inspect_exec(&exec.id).await.ok();
            let exit_code = inspect.and_then(|i| i.exit_code);

            info!(
                container_id = %container_id,
                exit_code = ?exit_code,
                "Container exec completed"
            );

            Ok(Json(ExecResponse {
                exit_code,
                stdout,
                stderr,
            }))
        }
        Ok(Err(e)) => Err(problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
            .with_title("Exec Failed")
            .with_detail(format!("Exec error: {}", e))),
        Err(_) => Err(problemdetails::new(StatusCode::GATEWAY_TIMEOUT)
            .with_title("Exec Timeout")
            .with_detail(format!("Command timed out after {} seconds", timeout))),
    }
}

/// Persistent terminal session via WebSocket (xterm.js compatible)
///
/// Protocol:
/// - Client sends binary frames -> written to container stdin (PTY)
/// - Server sends binary frames -> raw PTY output for xterm.js
/// - Client sends text `{"type":"resize","cols":N,"rows":N}` -> resize PTY
/// - Server sends text `{"type":"exit","code":N}` when exec ends
#[utoipa::path(
    tag = "Containers",
    get,
    path = "/projects/{project_id}/environments/{environment_id}/containers/{container_id}/terminal",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("environment_id" = i32, Path, description = "Environment ID"),
        ("container_id" = String, Path, description = "Container ID"),
    ),
    responses(
        (status = 101, description = "WebSocket connection established for terminal"),
        (status = 400, description = "Exec not enabled"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn container_terminal(
    State(state): State<Arc<AppState>>,
    Path((project_id, environment_id, container_id)): Path<(i32, i32, String)>,
    RequireAuth(auth): RequireAuth,
    ws: WebSocketUpgrade,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ContainersExec);

    // Verify the container belongs to this project/environment
    let (container_record, _env) = state
        .deployment_service
        .get_container_detail(project_id, environment_id, container_id.clone())
        .await
        .map_err(|_| {
            problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Container Not Found")
                .with_detail(format!(
                    "Container {} not found in project {} environment {}",
                    container_id, project_id, environment_id
                ))
        })?;

    // Use the verified container ID from the database record
    let verified_container_id = container_record.container_id;
    let node_id = container_record.node_id;

    info!(
        container_id = %verified_container_id,
        user = %auth.user_id(),
        node_id = ?node_id,
        "Terminal session requested"
    );

    // Remote container — proxy bytes 1:1 between the browser WS and the
    // agent WS. Resolve URL+token before the upgrade so we can fail fast
    // with a Problem instead of a half-open WebSocket.
    if let Some(nid) = node_id {
        let remote = state
            .deployment_service
            .resolve_remote_terminal(nid, &verified_container_id)
            .await
            .map_err(|e| {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Terminal Setup Failed")
                    .with_detail(e.to_string())
            })?;
        return Ok(ws.on_upgrade(move |socket| {
            handle_remote_terminal_proxy(socket, remote.ws_url, remote.token)
        }));
    }

    let docker = state.docker.clone();
    Ok(ws.on_upgrade(move |socket| handle_terminal_session(socket, docker, verified_container_id)))
}

/// Bidirectionally proxy a browser WebSocket to a worker agent's terminal
/// WebSocket. Each side forwards binary, text, and close frames verbatim.
/// The agent speaks the same xterm.js-friendly protocol the browser
/// expects, so no translation happens here.
async fn handle_remote_terminal_proxy(
    mut browser_socket: WebSocket,
    agent_ws_url: String,
    agent_token: String,
) {
    use futures::SinkExt as _;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::http::header::AUTHORIZATION;
    use tokio_tungstenite::tungstenite::protocol::Message as TMessage;

    let mut req = match agent_ws_url.as_str().into_client_request() {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(url = %agent_ws_url, "Invalid agent terminal URL: {}", e);
            let _ = browser_socket.close().await;
            return;
        }
    };
    req.headers_mut().insert(
        AUTHORIZATION,
        match format!("Bearer {}", agent_token).parse() {
            Ok(v) => v,
            Err(e) => {
                tracing::error!("Invalid agent token header: {}", e);
                let _ = browser_socket.close().await;
                return;
            }
        },
    );

    let (agent_stream, _resp) = match tokio_tungstenite::connect_async(req).await {
        Ok(ok) => ok,
        Err(e) => {
            tracing::error!(url = %agent_ws_url, "Agent terminal connect failed: {}", e);
            let _ = browser_socket.close().await;
            return;
        }
    };

    let (mut agent_tx, mut agent_rx) = agent_stream.split();
    let (mut browser_tx, mut browser_rx) = browser_socket.split();

    // browser -> agent
    let b2a = tokio::spawn(async move {
        while let Some(Ok(msg)) = browser_rx.next().await {
            let out = match msg {
                Message::Binary(b) => TMessage::Binary(b.to_vec()),
                Message::Text(t) => TMessage::Text(t.to_string()),
                Message::Close(_) => TMessage::Close(None),
                Message::Ping(p) => TMessage::Ping(p.to_vec()),
                Message::Pong(p) => TMessage::Pong(p.to_vec()),
            };
            if agent_tx.send(out).await.is_err() {
                break;
            }
        }
        let _ = agent_tx.close().await;
    });

    // agent -> browser
    let a2b = tokio::spawn(async move {
        while let Some(Ok(msg)) = agent_rx.next().await {
            let out = match msg {
                TMessage::Binary(b) => Message::Binary(b.to_vec().into()),
                TMessage::Text(t) => Message::Text(t.to_string().into()),
                TMessage::Close(_) => {
                    let _ = browser_tx.close().await;
                    return;
                }
                TMessage::Ping(p) => Message::Ping(p.to_vec().into()),
                TMessage::Pong(p) => Message::Pong(p.to_vec().into()),
                TMessage::Frame(_) => continue,
            };
            if browser_tx.send(out).await.is_err() {
                break;
            }
        }
        let _ = browser_tx.close().await;
    });

    // First side that finishes ends the session.
    tokio::select! {
        _ = b2a => {}
        _ = a2b => {}
    }
}

/// Handle a persistent terminal WebSocket session
async fn handle_terminal_session(
    socket: WebSocket,
    docker: Arc<bollard::Docker>,
    container_id: String,
) {
    debug!(container_id = %container_id, "Terminal session started");

    // Create exec with TTY — try bash, fall back to sh
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
            error!(error = %e, "Failed to create exec for terminal");
            return;
        }
    };

    let exec_id = exec.id.clone();

    // Start exec attached with TTY
    let start_config = bollard::exec::StartExecOptions {
        detach: false,
        tty: true,
        ..Default::default()
    };

    let (mut docker_output, mut docker_input) =
        match docker.start_exec(&exec_id, Some(start_config)).await {
            Ok(StartExecResults::Attached { output, input }) => (output, input),
            Ok(StartExecResults::Detached) => {
                error!("Exec started in detached mode unexpectedly");
                return;
            }
            Err(e) => {
                error!(error = %e, "Failed to start exec for terminal");
                return;
            }
        };

    let (mut ws_sender, mut ws_receiver) = socket.split();

    // Spawn task: Docker PTY output -> WebSocket binary frames for xterm.js
    let exec_id_for_output = exec_id.clone();
    let docker_for_output = docker.clone();
    let output_task = tokio::spawn(async move {
        use futures::SinkExt;

        while let Some(Ok(msg)) = docker_output.next().await {
            let bytes: Bytes = match msg {
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

        // Send exit message
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

    // Main loop: WebSocket input -> Docker PTY stdin
    let idle_timeout = tokio::time::Duration::from_secs(15 * 60); // 15 min
    loop {
        let msg = tokio::time::timeout(idle_timeout, ws_receiver.next()).await;

        match msg {
            Ok(Some(Ok(Message::Binary(data)))) => {
                // Raw keyboard input from xterm.js
                if docker_input.write_all(&data).await.is_err() {
                    break;
                }
                if docker_input.flush().await.is_err() {
                    break;
                }
            }
            Ok(Some(Ok(Message::Text(text)))) => {
                // Control messages (resize) or text input
                if let Ok(ctrl) = serde_json::from_str::<TerminalControl>(&text) {
                    match ctrl.r#type.as_str() {
                        "resize" => {
                            if let (Some(cols), Some(rows)) = (ctrl.cols, ctrl.rows) {
                                let resize_opts = bollard::exec::ResizeExecOptions {
                                    width: cols,
                                    height: rows,
                                };
                                if let Err(e) = docker.resize_exec(&exec_id, resize_opts).await {
                                    warn!(error = %e, "Failed to resize terminal");
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
                } else {
                    // Plain text input fallback
                    if docker_input.write_all(text.as_bytes()).await.is_err() {
                        break;
                    }
                    let _ = docker_input.flush().await;
                }
            }
            Ok(Some(Ok(Message::Close(_)))) | Ok(None) => {
                debug!(container_id = %container_id, "Terminal closed by client");
                break;
            }
            Err(_) => {
                info!(container_id = %container_id, "Terminal timed out (15 min idle)");
                break;
            }
            _ => {}
        }
    }

    output_task.abort();
    info!(container_id = %container_id, "Terminal session ended");
}

#[derive(Deserialize)]
struct TerminalControl {
    r#type: String,
    cols: Option<u16>,
    rows: Option<u16>,
    data: Option<String>,
}
