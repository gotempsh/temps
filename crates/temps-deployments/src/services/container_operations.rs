// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Location-aware operations for an already-scheduled container.
//!
//! Callers resolve this interface once from `deployment_containers.node_id`
//! and then perform lifecycle, log, exec, or terminal operations without
//! knowing whether the Docker daemon is local or exposed by a worker agent.

use async_trait::async_trait;
use axum::extract::ws::{Message, WebSocket};
use bollard::exec::StartExecResults;
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tracing::{debug, error, info, warn};

use super::{ContainerLogParams, ContainerLogStream, DeploymentError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerExecResult {
    pub exit_code: Option<i64>,
    pub stdout: String,
    pub stderr: String,
}

/// Operations against the runtime that owns a container.
///
/// `deployer` covers lifecycle and inspection operations shared by the local
/// Docker and remote-agent implementations. The remaining methods cover the
/// richer transports that cannot be represented by `ContainerDeployer`.
#[async_trait]
pub trait ContainerOperations: Send + Sync {
    fn deployer(&self) -> Arc<dyn temps_deployer::ContainerDeployer>;

    async fn stream_logs(
        &self,
        container_id: &str,
        params: ContainerLogParams,
    ) -> Result<ContainerLogStream, DeploymentError>;

    async fn exec(
        &self,
        container_id: &str,
        command: Vec<String>,
        timeout_seconds: u64,
    ) -> Result<ContainerExecResult, DeploymentError>;

    async fn serve_terminal(&self, socket: WebSocket, container_id: String);
}

pub struct LocalContainerOperations {
    docker: Arc<bollard::Docker>,
    log_service: Arc<temps_logs::DockerLogService>,
    deployer: Arc<dyn temps_deployer::ContainerDeployer>,
}

impl LocalContainerOperations {
    pub fn new(
        docker: Arc<bollard::Docker>,
        log_service: Arc<temps_logs::DockerLogService>,
        deployer: Arc<dyn temps_deployer::ContainerDeployer>,
    ) -> Self {
        Self {
            docker,
            log_service,
            deployer,
        }
    }

    fn operation_error(
        container_id: &str,
        operation: &'static str,
        reason: impl ToString,
    ) -> DeploymentError {
        DeploymentError::ContainerOperation {
            container_id: container_id.to_string(),
            operation,
            location: "local Docker runtime".to_string(),
            reason: reason.to_string(),
        }
    }
}

#[async_trait]
impl ContainerOperations for LocalContainerOperations {
    fn deployer(&self) -> Arc<dyn temps_deployer::ContainerDeployer> {
        self.deployer.clone()
    }

    async fn stream_logs(
        &self,
        container_id: &str,
        params: ContainerLogParams,
    ) -> Result<ContainerLogStream, DeploymentError> {
        let stream = self
            .log_service
            .get_container_logs(
                container_id,
                temps_logs::docker_logs::ContainerLogOptions {
                    start_date: params
                        .start_date
                        .and_then(|timestamp| chrono::DateTime::from_timestamp(timestamp, 0)),
                    end_date: params
                        .end_date
                        .and_then(|timestamp| chrono::DateTime::from_timestamp(timestamp, 0)),
                    tail: params.tail,
                    timestamps: params.timestamps,
                    follow: params.follow,
                },
            )
            .await
            .map_err(|error| Self::operation_error(container_id, "log stream", error))?;

        let mapped =
            stream.map(|item| item.map_err(|error| std::io::Error::other(error.to_string())));
        Ok(Box::pin(mapped))
    }

    async fn exec(
        &self,
        container_id: &str,
        command: Vec<String>,
        timeout_seconds: u64,
    ) -> Result<ContainerExecResult, DeploymentError> {
        let config = bollard::models::ExecConfig {
            attach_stdout: Some(true),
            attach_stderr: Some(true),
            cmd: Some(command),
            ..Default::default()
        };
        let exec = self
            .docker
            .create_exec(container_id, config)
            .await
            .map_err(|error| Self::operation_error(container_id, "exec creation", error))?;

        let docker = self.docker.clone();
        let exec_id = exec.id.clone();
        let output = tokio::time::timeout(
            std::time::Duration::from_secs(timeout_seconds),
            async move {
                let started = docker
                    .start_exec(
                        &exec_id,
                        Some(bollard::exec::StartExecOptions {
                            detach: false,
                            ..Default::default()
                        }),
                    )
                    .await?;
                let mut stdout = String::new();
                let mut stderr = String::new();
                if let StartExecResults::Attached { mut output, .. } = started {
                    while let Some(message) = output.next().await {
                        match message? {
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
            },
        )
        .await
        .map_err(|_| DeploymentError::ContainerExecTimeout {
            container_id: container_id.to_string(),
            timeout_seconds,
        })?
        .map_err(|error| Self::operation_error(container_id, "exec", error))?;

        let exit_code = self
            .docker
            .inspect_exec(&exec.id)
            .await
            .ok()
            .and_then(|inspection| inspection.exit_code);
        Ok(ContainerExecResult {
            exit_code,
            stdout: output.0,
            stderr: output.1,
        })
    }

    async fn serve_terminal(&self, socket: WebSocket, container_id: String) {
        serve_local_terminal(socket, self.docker.clone(), container_id).await;
    }
}

pub struct RemoteContainerOperations {
    node_id: i32,
    node_name: String,
    deployer: Arc<temps_deployer::remote::RemoteNodeDeployer>,
    http_client: reqwest::Client,
    terminal_connector: Option<tokio_tungstenite::Connector>,
}

impl RemoteContainerOperations {
    pub fn new(
        node_id: i32,
        node_name: String,
        deployer: Arc<temps_deployer::remote::RemoteNodeDeployer>,
        http_client: reqwest::Client,
        terminal_connector: Option<tokio_tungstenite::Connector>,
    ) -> Self {
        Self {
            node_id,
            node_name,
            deployer,
            http_client,
            terminal_connector,
        }
    }

    fn location(&self) -> String {
        format!("worker {} ({})", self.node_name, self.node_id)
    }

    fn operation_error(
        &self,
        container_id: &str,
        operation: &'static str,
        reason: impl ToString,
    ) -> DeploymentError {
        DeploymentError::ContainerOperation {
            container_id: container_id.to_string(),
            operation,
            location: self.location(),
            reason: reason.to_string(),
        }
    }

    fn log_stream_url(&self, container_id: &str, params: &ContainerLogParams) -> String {
        let mut url = format!(
            "{}/agent/containers/{}/logs/stream",
            self.deployer.agent_url().trim_end_matches('/'),
            container_id
        );
        let mut query = Vec::new();
        if let Some(value) = params.start_date {
            query.push(("start_date", value.to_string()));
        }
        if let Some(value) = params.end_date {
            query.push(("end_date", value.to_string()));
        }
        if let Some(value) = &params.tail {
            query.push(("tail", value.clone()));
        }
        query.push(("timestamps", params.timestamps.to_string()));
        query.push(("follow", params.follow.to_string()));
        let encoded = query
            .into_iter()
            .map(|(key, value)| format!("{}={}", key, urlencoding::encode(&value)))
            .collect::<Vec<_>>()
            .join("&");
        url.push('?');
        url.push_str(&encoded);
        url
    }

    fn terminal_url(&self, container_id: &str) -> Result<String, DeploymentError> {
        let base = self.deployer.agent_url().trim_end_matches('/');
        let ws_base = terminal_base(base).ok_or_else(|| {
            self.operation_error(
                container_id,
                "terminal setup",
                format!("agent URL has unsupported scheme: {base}"),
            )
        })?;
        Ok(format!(
            "{ws_base}/agent/containers/{container_id}/terminal"
        ))
    }
}

fn terminal_base(base: &str) -> Option<String> {
    base.strip_prefix("https://")
        .map(|rest| format!("wss://{rest}"))
        .or_else(|| {
            base.strip_prefix("http://")
                .map(|rest| format!("ws://{rest}"))
        })
}

#[async_trait]
impl ContainerOperations for RemoteContainerOperations {
    fn deployer(&self) -> Arc<dyn temps_deployer::ContainerDeployer> {
        self.deployer.clone()
    }

    async fn stream_logs(
        &self,
        container_id: &str,
        params: ContainerLogParams,
    ) -> Result<ContainerLogStream, DeploymentError> {
        let url = self.log_stream_url(container_id, &params);
        let response = self
            .http_client
            .get(&url)
            .bearer_auth(self.deployer.token())
            .send()
            .await
            .map_err(|error| self.operation_error(container_id, "log stream", error))?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(self.operation_error(
                container_id,
                "log stream",
                format!("agent returned {status}: {body}"),
            ));
        }

        let stream = response
            .bytes_stream()
            .map(|chunk| match chunk {
                Ok(bytes) => Ok(bytes
                    .iter()
                    .copied()
                    .filter(|byte| *byte != 0)
                    .collect::<Vec<_>>()),
                Err(error) => Err(std::io::Error::other(format!(
                    "Remote log stream error: {error}"
                ))),
            })
            .filter_map(|result| async move {
                match result {
                    Ok(bytes) if bytes.is_empty() => None,
                    Ok(bytes) => Some(Ok(String::from_utf8_lossy(&bytes).to_string())),
                    Err(error) => Some(Err(error)),
                }
            });
        Ok(Box::pin(stream))
    }

    async fn exec(
        &self,
        container_id: &str,
        command: Vec<String>,
        timeout_seconds: u64,
    ) -> Result<ContainerExecResult, DeploymentError> {
        let result = self
            .deployer
            .exec_command(container_id, command, Some(timeout_seconds))
            .await
            .map_err(|error| self.operation_error(container_id, "exec", error))?;
        Ok(ContainerExecResult {
            exit_code: result.exit_code,
            stdout: result.stdout,
            stderr: result.stderr,
        })
    }

    async fn serve_terminal(&self, socket: WebSocket, container_id: String) {
        let url = match self.terminal_url(&container_id) {
            Ok(url) => url,
            Err(error) => {
                error!(%error, "Could not resolve remote terminal URL");
                return;
            }
        };
        serve_remote_terminal(
            socket,
            url,
            self.deployer.token().to_string(),
            self.terminal_connector.clone(),
        )
        .await;
    }
}

async fn serve_remote_terminal(
    mut browser_socket: WebSocket,
    agent_ws_url: String,
    agent_token: String,
    connector: Option<tokio_tungstenite::Connector>,
) {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::http::header::AUTHORIZATION;
    use tokio_tungstenite::tungstenite::protocol::Message as AgentMessage;

    let mut request = match agent_ws_url.as_str().into_client_request() {
        Ok(request) => request,
        Err(error) => {
            error!(url = %agent_ws_url, %error, "Invalid agent terminal URL");
            let _ = browser_socket.close().await;
            return;
        }
    };
    let authorization = match format!("Bearer {agent_token}").parse() {
        Ok(value) => value,
        Err(error) => {
            error!(%error, "Invalid agent terminal authorization header");
            let _ = browser_socket.close().await;
            return;
        }
    };
    request.headers_mut().insert(AUTHORIZATION, authorization);

    let (agent_stream, _) =
        match tokio_tungstenite::connect_async_tls_with_config(request, None, false, connector)
            .await
        {
            Ok(connection) => connection,
            Err(error) => {
                error!(url = %agent_ws_url, %error, "Agent terminal connection failed");
                let _ = browser_socket.close().await;
                return;
            }
        };

    let (mut agent_tx, mut agent_rx) = agent_stream.split();
    let (mut browser_tx, mut browser_rx) = browser_socket.split();
    let browser_to_agent = tokio::spawn(async move {
        while let Some(Ok(message)) = browser_rx.next().await {
            let message = match message {
                Message::Binary(value) => AgentMessage::Binary(value),
                Message::Text(value) => AgentMessage::Text(value.to_string().into()),
                Message::Close(_) => AgentMessage::Close(None),
                Message::Ping(value) => AgentMessage::Ping(value),
                Message::Pong(value) => AgentMessage::Pong(value),
            };
            if agent_tx.send(message).await.is_err() {
                break;
            }
        }
        let _ = agent_tx.close().await;
    });
    let agent_to_browser = tokio::spawn(async move {
        while let Some(Ok(message)) = agent_rx.next().await {
            let message = match message {
                AgentMessage::Binary(value) => Message::Binary(value.to_vec().into()),
                AgentMessage::Text(value) => Message::Text(value.to_string().into()),
                AgentMessage::Close(_) => {
                    let _ = browser_tx.close().await;
                    return;
                }
                AgentMessage::Ping(value) => Message::Ping(value.to_vec().into()),
                AgentMessage::Pong(value) => Message::Pong(value.to_vec().into()),
                AgentMessage::Frame(_) => continue,
            };
            if browser_tx.send(message).await.is_err() {
                break;
            }
        }
        let _ = browser_tx.close().await;
    });
    tokio::select! {
        _ = browser_to_agent => {}
        _ = agent_to_browser => {}
    }
}

async fn serve_local_terminal(
    socket: WebSocket,
    docker: Arc<bollard::Docker>,
    container_id: String,
) {
    debug!(container_id = %container_id, "Terminal session started");
    let exec = match docker
        .create_exec(
            &container_id,
            bollard::models::ExecConfig {
                attach_stdin: Some(true),
                attach_stdout: Some(true),
                attach_stderr: Some(true),
                tty: Some(true),
                cmd: Some(vec![
                    "/bin/sh".to_string(),
                    "-c".to_string(),
                    "if command -v bash >/dev/null 2>&1; then exec bash; else exec sh; fi"
                        .to_string(),
                ]),
                ..Default::default()
            },
        )
        .await
    {
        Ok(exec) => exec,
        Err(error) => {
            error!(container_id = %container_id, %error, "Failed to create terminal exec");
            return;
        }
    };
    let exec_id = exec.id;
    let (mut docker_output, mut docker_input) = match docker
        .start_exec(
            &exec_id,
            Some(bollard::exec::StartExecOptions {
                detach: false,
                tty: true,
                ..Default::default()
            }),
        )
        .await
    {
        Ok(StartExecResults::Attached { output, input }) => (output, input),
        Ok(StartExecResults::Detached) => {
            error!(container_id = %container_id, "Terminal exec unexpectedly detached");
            return;
        }
        Err(error) => {
            error!(container_id = %container_id, %error, "Failed to start terminal exec");
            return;
        }
    };

    let (mut ws_sender, mut ws_receiver) = socket.split();
    let output_exec_id = exec_id.clone();
    let output_docker = docker.clone();
    let output_task = tokio::spawn(async move {
        while let Some(Ok(message)) = docker_output.next().await {
            let bytes: Bytes = match message {
                bollard::container::LogOutput::StdOut { message }
                | bollard::container::LogOutput::StdErr { message }
                | bollard::container::LogOutput::Console { message } => message,
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
        let exit_code = output_docker
            .inspect_exec(&output_exec_id)
            .await
            .ok()
            .and_then(|inspection| inspection.exit_code)
            .unwrap_or(-1);
        let _ = ws_sender
            .send(Message::Text(
                format!(r#"{{"type":"exit","code":{exit_code}}}"#).into(),
            ))
            .await;
        let _ = ws_sender.close().await;
    });

    let idle_timeout = tokio::time::Duration::from_secs(15 * 60);
    loop {
        match tokio::time::timeout(idle_timeout, ws_receiver.next()).await {
            Ok(Some(Ok(Message::Binary(data)))) => {
                if docker_input.write_all(&data).await.is_err()
                    || docker_input.flush().await.is_err()
                {
                    break;
                }
            }
            Ok(Some(Ok(Message::Text(text)))) => {
                if let Ok(control) = serde_json::from_str::<TerminalControl>(&text) {
                    match control.r#type.as_str() {
                        "resize" => {
                            if let (Some(cols), Some(rows)) = (control.cols, control.rows) {
                                if let Err(error) = docker
                                    .resize_exec(
                                        &exec_id,
                                        bollard::exec::ResizeExecOptions {
                                            width: cols,
                                            height: rows,
                                        },
                                    )
                                    .await
                                {
                                    warn!(container_id = %container_id, %error, "Failed to resize terminal");
                                }
                            }
                        }
                        "input" => {
                            if let Some(data) = control.data {
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
            Ok(Some(Ok(Message::Close(_)))) | Ok(None) => break,
            Err(_) => {
                info!(container_id = %container_id, "Terminal timed out after 15 idle minutes");
                break;
            }
            _ => {}
        }
    }
    output_task.abort();
    info!(container_id = %container_id, "Terminal session ended");
}

#[derive(serde::Deserialize)]
struct TerminalControl {
    r#type: String,
    cols: Option<u16>,
    rows: Option<u16>,
    data: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_url_maps_http_schemes() {
        assert_eq!(
            terminal_base("http://10.0.0.2:3100").expect("http URL"),
            "ws://10.0.0.2:3100"
        );
        assert_eq!(
            terminal_base("https://worker.example.test").expect("https URL"),
            "wss://worker.example.test"
        );
    }

    #[test]
    fn terminal_url_rejects_unknown_scheme() {
        assert!(terminal_base("ftp://worker.example.test").is_none());
    }
}
