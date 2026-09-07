// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Container exec and terminal handlers.
//!
//! Provides one-shot exec (POST) and persistent terminal (WebSocket) access
//! to running containers. Both require the `ContainersExec` permission and
//! the project must have `container_exec_enabled` set to true.

use axum::{
    extract::{ws::WebSocketUpgrade, Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_auth::{permission_guard, project_access_guard, project_scope_guard, RequireAuth};
use temps_core::problemdetails::{self, Problem};
use tracing::info;
use utoipa::ToSchema;

use super::types::AppState;

async fn verify_container_exec_access(
    state: &AppState,
    auth: &temps_auth::AuthContext,
    project_id: i32,
    environment_id: i32,
    container_id: String,
) -> Result<temps_entities::deployment_containers::Model, Problem> {
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);

    // Verify the container belongs to this project/environment before using
    // the caller-supplied Docker ID against any Docker daemon.
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

    if let Some(token) = auth.deployment_token_info() {
        if token
            .environment_id
            .is_some_and(|token_environment_id| token_environment_id != environment_id)
            || token.deployment_id.is_some_and(|token_deployment_id| {
                token_deployment_id != container_record.deployment_id
            })
        {
            return Err(problemdetails::new(StatusCode::FORBIDDEN)
                .with_title("Deployment Token Scope Denied")
                .with_detail(
                    "This deployment token is not scoped to the requested container's environment or deployment",
                ));
        }
    }

    let exec_enabled = state
        .deployment_service
        .is_container_exec_enabled(project_id, environment_id)
        .await
        .map_err(|_| {
            problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Environment Not Found")
                .with_detail(format!(
                    "Environment {} was not found in project {}",
                    environment_id, project_id
                ))
        })?;

    if !exec_enabled {
        return Err(problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Container Exec Disabled")
            .with_detail(
                "Container exec and terminal access must be enabled for this environment before use",
            ));
    }

    Ok(container_record)
}

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

    let container_record = verify_container_exec_access(
        &state,
        &auth,
        project_id,
        environment_id,
        container_id.clone(),
    )
    .await?;

    let verified_container_id = container_record.container_id;
    let timeout = std::cmp::min(request.timeout_seconds.unwrap_or(30), 300);
    let operations = state
        .deployment_service
        .container_operations_for_node(container_record.node_id)
        .await
        .map_err(Problem::from)?;
    let result = operations
        .exec(&verified_container_id, request.command, timeout)
        .await
        .map_err(Problem::from)?;

    info!(
        container_id = %container_id,
        node_id = ?container_record.node_id,
        exit_code = ?result.exit_code,
        "Container exec completed"
    );

    Ok(Json(ExecResponse {
        exit_code: result.exit_code,
        stdout: result.stdout,
        stderr: result.stderr,
    }))
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

    let container_record = verify_container_exec_access(
        &state,
        &auth,
        project_id,
        environment_id,
        container_id.clone(),
    )
    .await?;

    // Use the verified container ID from the database record
    let verified_container_id = container_record.container_id;
    let node_id = container_record.node_id;

    info!(
        container_id = %verified_container_id,
        user = %auth.user_id(),
        node_id = ?node_id,
        "Terminal session requested"
    );

    let operations = state
        .deployment_service
        .container_operations_for_node(node_id)
        .await
        .map_err(Problem::from)?;

    Ok(ws.on_upgrade(move |socket| async move {
        operations
            .serve_terminal(socket, verified_container_id)
            .await;
    }))
}

#[cfg(test)]
mod tests {
    /// Live mutual-TLS check of the terminal **WebSocket** transport (ADR-020
    /// WS-2.1). The terminal proxy dials the agent with tokio-tungstenite (not
    /// reqwest), so this exercises that distinct stack: a rustls `Connector`
    /// built from a CA-signed client identity, the TLS handshake, and the WS
    /// upgrade. A bogus container id is fine — any completed HTTP response (even
    /// 4xx) proves TLS + client-cert auth succeeded; only a transport/TLS error
    /// means mTLS failed. Skips unless `TEMPS_MTLS_*` is set; run inside a
    /// cluster container that can reach the agent.
    #[tokio::test]
    async fn test_terminal_ws_mtls_live() {
        use rustls::pki_types::{CertificateDer, PrivateKeyDer};
        use std::io::BufReader;
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        use tokio_tungstenite::tungstenite::http::header::AUTHORIZATION;

        let (url, token, cert, key, ca) = match (
            std::env::var("TEMPS_MTLS_WS_URL"),
            std::env::var("TEMPS_MTLS_TOKEN"),
            std::env::var("TEMPS_MTLS_CERT"),
            std::env::var("TEMPS_MTLS_KEY"),
            std::env::var("TEMPS_MTLS_CA"),
        ) {
            (Ok(u), Ok(t), Ok(c), Ok(k), Ok(a)) => (u, t, c, k, a),
            _ => {
                eprintln!("TEMPS_MTLS_* not set — skipping terminal WS mTLS live test");
                return;
            }
        };
        // Tests don't run the CLI bootstrap that installs the crypto provider.
        let _ = rustls::crypto::ring::default_provider().install_default();

        let cert_pem = std::fs::read(&cert).expect("read client cert");
        let key_pem = std::fs::read(&key).expect("read client key");
        let ca_pem = std::fs::read(&ca).expect("read cluster CA");

        let cert_chain: Vec<CertificateDer<'static>> =
            rustls_pemfile::certs(&mut BufReader::new(&cert_pem[..]))
                .collect::<Result<_, _>>()
                .expect("parse cert chain");
        let pkey: PrivateKeyDer<'static> =
            rustls_pemfile::private_key(&mut BufReader::new(&key_pem[..]))
                .expect("parse key")
                .expect("key present");
        let mut roots = rustls::RootCertStore::empty();
        for c in rustls_pemfile::certs(&mut BufReader::new(&ca_pem[..])) {
            roots.add(c.expect("parse CA")).expect("add CA root");
        }
        let cfg = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_client_auth_cert(cert_chain, pkey)
            .expect("build client config");

        let mut req = url.as_str().into_client_request().expect("build request");
        req.headers_mut().insert(
            AUTHORIZATION,
            format!("Bearer {}", token).parse().expect("auth header"),
        );

        let connector = tokio_tungstenite::Connector::Rustls(std::sync::Arc::new(cfg));
        let res =
            tokio_tungstenite::connect_async_tls_with_config(req, None, false, Some(connector))
                .await;
        match res {
            Ok(_) => eprintln!("✓ terminal WS mTLS: handshake + upgrade OK ({url})"),
            Err(tokio_tungstenite::tungstenite::Error::Http(resp)) => eprintln!(
                "✓ terminal WS mTLS: TLS + client-cert auth OK, agent returned HTTP {} ({url})",
                resp.status()
            ),
            Err(e) => panic!("terminal WS mTLS FAILED at transport/TLS layer: {e}"),
        }
    }
}
