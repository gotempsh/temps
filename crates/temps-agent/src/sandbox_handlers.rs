// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Sandbox host API (ADR-048).
//!
//! Lets the control plane run sandboxes on this worker. Each endpoint
//! replays one `SandboxProvider` call against the worker's own
//! `DockerSandboxProvider` — the same implementation the control plane uses
//! for local sandboxes — so behaviour is identical wherever a sandbox lives.
//! The control plane's side is `temps_agents::sandbox::remote`.
//!
//! Trust boundary: the caller is the authenticated control plane (bearer
//! token + mTLS, enforced by the same middleware as every other agent
//! route). Even so, the worker never accepts a host path from the request:
//! the sandbox work directory is always `<agent data dir>/sandboxes/<label>`,
//! and every handle-based call is re-resolved against this node's Docker
//! daemon by sandbox *name*: the container id in the request is ignored, and
//! the container found must carry the sandbox label, so a handle cannot
//! point a sandbox operation at an application, service or sidecar
//! container.
//!
//! Resource bounds: uploads are limited in size (route body limit) and in
//! concurrency ([`limit_uploads`]); uploaded directories may only contain
//! regular files and directories; reads and exec output are capped before
//! they are returned.

use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use base64::Engine;
use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;

use temps_agents::ai_cli::OnEventCallback;
use temps_agents::error::AgentError;
use temps_agents::sandbox::docker::SANDBOX_CONTAINER_LABEL;
use temps_agents::sandbox::remote::{
    is_sandbox_handle, is_valid_sandbox_label, RemoteAliveResponse, RemoteCreateRequest,
    RemoteDestroyRequest, RemoteErrorBody, RemoteExecFrame, RemoteExecRequest, RemoteExecResponse,
    RemoteFileContents, RemoteHandleRequest, RemoteKillRequest, RemoteOkResponse,
    RemoteReadFileRequest, RemoteRecoverRequest, RemoteStatusResponse, RemoteWriteDirectoryRequest,
    RemoteWriteFileRequest, EXEC_STREAM_HEARTBEAT_INTERVAL, SANDBOX_CONTAINER_PREFIX,
    WORKER_EXEC_OUTPUT_LIMIT, WORKER_READ_FILE_MAX_BYTES,
};
use temps_agents::sandbox::{
    ExecStream, OnStreamEventCallback, SandboxCreateConfig, SandboxHandle, SandboxProvider,
};

/// Request body cap for the two sandbox upload routes (`write-file`,
/// `write-directory`); directory uploads are a tar archive, base64-encoded
/// inside JSON (~4/3 overhead). Every other sandbox route keeps axum's
/// default 2 MiB cap so it cannot be used to make the agent buffer huge
/// bodies.
pub const SANDBOX_UPLOAD_BODY_LIMIT: usize = 512 * 1024 * 1024;

/// Uploads handled at once. Each one can hold its JSON body, the decoded
/// bytes and an archive copy in memory (~1.3 GB at the body limit), so the
/// worker serialises them instead of letting parallel uploads exhaust it.
pub const MAX_CONCURRENT_UPLOADS: usize = 2;

/// How long an upload waits for a free slot before the worker answers 503.
const UPLOAD_PERMIT_WAIT: Duration = Duration::from_secs(30);

/// Most entries an uploaded directory archive may contain.
const MAX_UPLOAD_ENTRIES: usize = 100_000;

/// Most bytes an uploaded directory archive may unpack to. No bigger than
/// the upload itself: only regular files are accepted, and their data has to
/// be in the archive.
const MAX_UPLOAD_UNPACKED_BYTES: u64 = SANDBOX_UPLOAD_BODY_LIMIT as u64;

/// What a node's Docker daemon has under an exact container name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NamedContainer {
    /// No container has that name.
    Missing,
    /// A sandbox container (it carries [`SANDBOX_CONTAINER_LABEL`]).
    Sandbox { id: String, running: bool },
    /// A container with that name that is not a sandbox — the egress proxy
    /// sidecar, or an app whose name happens to share the prefix.
    Other,
}

/// Finds containers by exact name on this node's Docker daemon.
#[async_trait::async_trait]
pub trait ContainerLookup: Send + Sync {
    async fn find(&self, name: &str) -> Result<NamedContainer, String>;
}

#[async_trait::async_trait]
impl ContainerLookup for bollard::Docker {
    async fn find(&self, name: &str) -> Result<NamedContainer, String> {
        match self
            .inspect_container(
                name,
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await
        {
            // Docker also resolves ids and id prefixes here; only an exact
            // name match counts.
            Ok(info) if info.name.as_deref().map(|n| n.trim_start_matches('/')) == Some(name) => {
                let is_sandbox = info
                    .config
                    .as_ref()
                    .and_then(|config| config.labels.as_ref())
                    .and_then(|labels| labels.get(SANDBOX_CONTAINER_LABEL))
                    .is_some_and(|value| value == "true");
                match (is_sandbox, info.id) {
                    (true, Some(id)) => Ok(NamedContainer::Sandbox {
                        id,
                        running: info.state.and_then(|s| s.running).unwrap_or(false),
                    }),
                    (true, None) | (false, _) => Ok(NamedContainer::Other),
                }
            }
            Ok(_) => Ok(NamedContainer::Missing),
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => Ok(NamedContainer::Missing),
            Err(e) => Err(format!("inspect container '{}': {}", name, e)),
        }
    }
}

/// What a node needs to host sandboxes: the provider and a way to check
/// which container a handle really names.
pub struct SandboxHost {
    pub provider: Arc<dyn SandboxProvider>,
    pub containers: Arc<dyn ContainerLookup>,
}

/// Shared state for the sandbox host routes.
pub struct SandboxHostState {
    /// `None` when this node has no usable Docker daemon; every route then
    /// answers 503 with the reason instead of pretending to work.
    host: Option<SandboxHost>,
    /// Root for sandbox work directories on this node.
    work_root: PathBuf,
    /// Labels with a create in flight; a second create for one is refused.
    creating: std::sync::Mutex<HashSet<String>>,
    /// Upload slots, see [`MAX_CONCURRENT_UPLOADS`].
    upload_permits: Arc<Semaphore>,
    upload_permit_wait: Duration,
}

impl SandboxHostState {
    pub fn new(host: Option<SandboxHost>, work_root: PathBuf) -> Self {
        Self {
            host,
            work_root,
            creating: std::sync::Mutex::new(HashSet::new()),
            upload_permits: Arc::new(Semaphore::new(MAX_CONCURRENT_UPLOADS)),
            upload_permit_wait: UPLOAD_PERMIT_WAIT,
        }
    }

    /// Shorten the upload slot wait (tests).
    #[cfg(test)]
    fn with_upload_permit_wait(mut self, wait: Duration) -> Self {
        self.upload_permit_wait = wait;
        self
    }
}

type HostState = State<Arc<SandboxHostState>>;
type ApiError = (StatusCode, Json<RemoteErrorBody>);
type ApiResult<T> = Result<Json<T>, ApiError>;

fn err(status: StatusCode, message: impl Into<String>) -> ApiError {
    (
        status,
        Json(RemoteErrorBody {
            error: message.into(),
        }),
    )
}

fn host(state: &SandboxHostState) -> Result<&SandboxHost, ApiError> {
    state.host.as_ref().ok_or_else(|| {
        err(
            StatusCode::SERVICE_UNAVAILABLE,
            "sandboxes are unavailable on this node: the agent has no Docker daemon connection",
        )
    })
}

/// Rebuild a control-plane handle against this node. Only the sandbox name
/// is taken from the request; the container id comes from Docker, looked up
/// by that name, and the container must be a labelled sandbox. `Ok(None)` =
/// no such container on this node.
async fn resolve_handle(
    host: &SandboxHost,
    mut handle: SandboxHandle,
) -> Result<Option<SandboxHandle>, ApiError> {
    if !is_sandbox_handle(&handle) {
        return Err(err(
            StatusCode::BAD_REQUEST,
            format!("'{}' is not a sandbox container", handle.sandbox_name),
        ));
    }
    match host.containers.find(&handle.sandbox_name).await {
        Ok(NamedContainer::Sandbox { id, .. }) => {
            handle.sandbox_id = id;
            Ok(Some(handle))
        }
        Ok(NamedContainer::Missing) => Ok(None),
        Ok(NamedContainer::Other) => {
            tracing::warn!(
                container = %handle.sandbox_name,
                "Refused a sandbox call for a container without the sandbox label"
            );
            Err(err(
                StatusCode::BAD_REQUEST,
                format!(
                    "container '{}' on this node is not a sandbox (no {} label)",
                    handle.sandbox_name, SANDBOX_CONTAINER_LABEL
                ),
            ))
        }
        Err(e) => Err(err(
            StatusCode::SERVICE_UNAVAILABLE,
            format!(
                "the Docker daemon on this node failed while resolving sandbox '{}': {}",
                handle.sandbox_name, e
            ),
        )),
    }
}

/// [`resolve_handle`], treating a missing container as a 404.
async fn require_handle(
    host: &SandboxHost,
    handle: SandboxHandle,
) -> Result<SandboxHandle, ApiError> {
    let name = handle.sandbox_name.clone();
    resolve_handle(host, handle).await?.ok_or_else(|| {
        err(
            StatusCode::NOT_FOUND,
            format!("sandbox container '{}' does not exist on this node", name),
        )
    })
}

/// Map a provider error to the status the control plane's client reads:
/// 404 = the container is gone, 400 = bad request, 422 = not available on a
/// worker node, 503 = this node can't serve sandboxes right now, 500 = the
/// operation itself failed.
fn provider_err(e: AgentError) -> ApiError {
    let status = match &e {
        AgentError::SandboxNotFound { .. } => StatusCode::NOT_FOUND,
        AgentError::Validation { .. }
        | AgentError::ImmutableSandboxImageRebuild { .. }
        | AgentError::SnapshotSizeLimitExceeded { .. } => StatusCode::BAD_REQUEST,
        AgentError::SandboxUnsupportedOnNode { .. } => StatusCode::UNPROCESSABLE_ENTITY,
        AgentError::SandboxConflictOnNode { .. } => StatusCode::CONFLICT,
        AgentError::SandboxProviderUnavailable { .. }
        | AgentError::SandboxNodeUnavailable { .. } => StatusCode::SERVICE_UNAVAILABLE,
        AgentError::SandboxCreationFailed { .. }
        | AgentError::SandboxExecFailed { .. }
        | AgentError::Io(_)
        | AgentError::Database(_) => StatusCode::INTERNAL_SERVER_ERROR,
        // Agent-run, definition and secret errors are never produced by the
        // sandbox provider. If one ever is, it is an unexpected failure of
        // the operation — never "the sandbox is gone" (which a 404 would
        // tell the control plane) or a caller mistake.
        AgentError::ConfigNotFound { .. }
        | AgentError::AgentNotFound { .. }
        | AgentError::RunNotFound { .. }
        | AgentError::ProjectNotFound { .. }
        | AgentError::BudgetExceeded { .. }
        | AgentError::CooldownActive { .. }
        | AgentError::AiCliNotInstalled { .. }
        | AgentError::AiCliFailed { .. }
        | AgentError::AiCliReportedError { .. }
        | AgentError::AiCliTimeout { .. }
        | AgentError::GitError { .. }
        | AgentError::EncryptionError { .. }
        | AgentError::SecretNotFound { .. }
        | AgentError::SkillDefinitionNotFound { .. }
        | AgentError::McpDefinitionNotFound { .. }
        | AgentError::McpConfigFieldNotFound { .. }
        | AgentError::SkillDefinitionAlreadyExists { .. }
        | AgentError::McpDefinitionAlreadyExists { .. }
        | AgentError::DockerSocketWriteRequiresAdmin { .. } => StatusCode::INTERNAL_SERVER_ERROR,
    };
    err(status, e.to_string())
}

fn ok() -> Json<RemoteOkResponse> {
    Json(RemoteOkResponse { ok: true })
}

fn work_dir_for(state: &SandboxHostState, label: &str) -> PathBuf {
    state.work_root.join(label)
}

fn label_of(handle: &SandboxHandle) -> Option<&str> {
    handle.sandbox_name.strip_prefix(SANDBOX_CONTAINER_PREFIX)
}

/// Middleware for the upload routes: at most [`MAX_CONCURRENT_UPLOADS`]
/// run at once. Taken before the body is read, so waiting uploads hold no
/// memory; when no slot frees up in time the worker answers 503 and the
/// control plane reports the node as busy.
pub async fn limit_uploads(
    State(state): State<Arc<SandboxHostState>>,
    request: Request,
    next: Next,
) -> Response {
    let path = request.uri().path().to_string();
    match tokio::time::timeout(
        state.upload_permit_wait,
        state.upload_permits.clone().acquire_owned(),
    )
    .await
    {
        Ok(Ok(permit)) => {
            let response = next.run(request).await;
            drop(permit);
            response
        }
        Ok(Err(_)) | Err(_) => {
            tracing::warn!(
                route = %path,
                max_concurrent = MAX_CONCURRENT_UPLOADS,
                "Refused a sandbox upload: every upload slot stayed busy"
            );
            err(
                StatusCode::SERVICE_UNAVAILABLE,
                format!(
                    "{} is busy: {} sandbox uploads are already in progress on this node \
                     and none finished within {}s; retry shortly",
                    path,
                    MAX_CONCURRENT_UPLOADS,
                    state.upload_permit_wait.as_secs()
                ),
            )
            .into_response()
        }
    }
}

/// Largest error body the path redaction reads. Error bodies are one short
/// message; anything bigger is passed through untouched rather than buffered.
const REDACT_BODY_LIMIT: usize = 64 * 1024;

/// What a redacted host path is replaced with in error messages.
const REDACTED_WORK_ROOT: &str = "<sandbox work dir>";

/// Replace this node's sandbox work root in `message` with a placeholder.
fn redact_work_root(message: &str, work_root: &Path) -> Option<String> {
    let root = work_root.to_string_lossy();
    let root = root.trim_end_matches('/');
    if root.is_empty() || !message.contains(root) {
        return None;
    }
    Some(message.replace(root, REDACTED_WORK_ROOT))
}

/// Middleware for every sandbox route: error messages travel to the control
/// plane and on to sandbox owners, who must not learn this node's
/// filesystem layout. Any occurrence of the work root (which also contains
/// the per-sandbox work and staging directories) in an error body is
/// replaced with a placeholder; the full message is logged here for the
/// operator.
pub async fn redact_host_paths(
    State(state): State<Arc<SandboxHostState>>,
    request: Request,
    next: Next,
) -> Response {
    let path = request.uri().path().to_string();
    let response = next.run(request).await;
    if response.status().is_success() {
        return response;
    }
    let (mut parts, body) = response.into_parts();
    let bytes = match axum::body::to_bytes(body, REDACT_BODY_LIMIT).await {
        Ok(bytes) => bytes,
        Err(e) => {
            tracing::warn!(route = %path, error = %e, "Could not read a sandbox error body to redact it");
            parts.headers.remove(axum::http::header::CONTENT_LENGTH);
            return Response::from_parts(
                parts,
                axum::body::Body::from(
                    r#"{"error":"the sandbox operation failed; see the node's agent log"}"#,
                ),
            );
        }
    };
    let text = String::from_utf8_lossy(&bytes);
    match redact_work_root(&text, &state.work_root) {
        Some(redacted) => {
            tracing::warn!(route = %path, status = parts.status.as_u16(), error = %text, "Sandbox call failed");
            parts.headers.remove(axum::http::header::CONTENT_LENGTH);
            Response::from_parts(parts, axum::body::Body::from(redacted))
        }
        None => Response::from_parts(parts, axum::body::Body::from(bytes)),
    }
}

/// Marks a label as being created; released on drop, wherever the create
/// ends (including in the background task after the requester left).
struct CreateClaim {
    state: Arc<SandboxHostState>,
    label: String,
}

impl CreateClaim {
    fn acquire(state: &Arc<SandboxHostState>, label: &str) -> Result<Self, ApiError> {
        let mut creating = state.creating.lock().map_err(|_| {
            err(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(
                    "create sandbox '{}': the in-flight create registry is poisoned",
                    label
                ),
            )
        })?;
        if !creating.insert(label.to_string()) {
            return Err(err(
                StatusCode::CONFLICT,
                format!("sandbox '{}' is already being created on this node", label),
            ));
        }
        Ok(Self {
            state: state.clone(),
            label: label.to_string(),
        })
    }
}

impl Drop for CreateClaim {
    fn drop(&mut self) {
        if let Ok(mut creating) = self.state.creating.lock() {
            creating.remove(&self.label);
        }
    }
}

/// Run one create to completion, whether or not anyone still waits for it.
///
/// If the requester is gone when the sandbox is ready (the control plane
/// timed out or its connection dropped, which cancels the HTTP handler),
/// nothing would ever track the new container, so it is destroyed. Its
/// volumes go too when the sandbox is new (`fresh`); a replaced stopped
/// sandbox keeps them. The work directory is removed only when this
/// request created it.
async fn run_create(
    provider: Arc<dyn SandboxProvider>,
    config: SandboxCreateConfig,
    work_dir: PathBuf,
    created_work_dir: bool,
    fresh: bool,
    claim: CreateClaim,
    reply: tokio::sync::oneshot::Sender<Result<SandboxHandle, AgentError>>,
) {
    let label = claim.label.clone();
    let result = provider.create(config).await;
    if result.is_err() && created_work_dir {
        remove_work_dir(&work_dir).await;
    }
    if let Err(Ok(orphan)) = reply.send(result) {
        tracing::warn!(
            sandbox = %orphan.sandbox_name,
            label = %label,
            "Control plane stopped waiting for a sandbox create; destroying the new container so it is not left untracked"
        );
        if let Err(e) = provider.destroy(&orphan, fresh).await {
            tracing::error!(
                sandbox = %orphan.sandbox_name,
                error = %e,
                "Failed to destroy a sandbox whose create was abandoned by the control plane"
            );
        }
        if created_work_dir {
            remove_work_dir(&work_dir).await;
        }
    }
    drop(claim);
}

async fn remove_work_dir(dir: &Path) {
    if let Err(e) = tokio::fs::remove_dir_all(dir).await {
        if e.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(dir = %dir.display(), error = %e, "Failed to remove sandbox work dir");
        }
    }
}

/// `POST /agent/sandboxes`
///
/// Refuses (409) to touch a live sandbox: a running sandbox container, a
/// non-sandbox container with the same name, a create already in flight for
/// the label, or a leftover work directory with no container. A *stopped*
/// sandbox container is replaced keeping its volumes and work directory —
/// how a sandbox stopped for a stale isolation policy is recreated, exactly
/// as on the control plane.
#[utoipa::path(
    tag = "Sandboxes",
    post,
    path = "/agent/sandboxes",
    request_body = RemoteCreateRequest,
    responses(
        (status = 200, description = "Sandbox created", body = SandboxHandle),
        (status = 400, description = "Invalid label or create request", body = RemoteErrorBody),
        (status = 401, description = "Unauthorized"),
        (status = 409, description = "The sandbox already exists or is being created", body = RemoteErrorBody),
        (status = 500, description = "Create failed", body = RemoteErrorBody),
        (status = 503, description = "No Docker daemon on this node", body = RemoteErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn create_sandbox(
    State(state): HostState,
    Json(req): Json<RemoteCreateRequest>,
) -> ApiResult<SandboxHandle> {
    let host = host(&state)?;
    if !is_valid_sandbox_label(&req.label) {
        return Err(err(
            StatusCode::BAD_REQUEST,
            format!("invalid sandbox label '{}'", req.label),
        ));
    }
    let claim = CreateClaim::acquire(&state, &req.label)?;
    let container_name = format!("{}{}", SANDBOX_CONTAINER_PREFIX, req.label);
    let replacing = match host.containers.find(&container_name).await {
        Ok(NamedContainer::Missing) => false,
        Ok(NamedContainer::Sandbox { running: false, .. }) => true,
        Ok(NamedContainer::Sandbox { running: true, .. }) => {
            return Err(err(
                StatusCode::CONFLICT,
                format!(
                    "sandbox '{}' already exists and is running on this node; destroy it before creating it again",
                    req.label
                ),
            ));
        }
        Ok(NamedContainer::Other) => {
            return Err(err(
                StatusCode::CONFLICT,
                format!(
                    "a container named '{}' already exists on this node and is not a sandbox",
                    container_name
                ),
            ));
        }
        Err(e) => {
            return Err(err(
                StatusCode::SERVICE_UNAVAILABLE,
                format!(
                    "the Docker daemon on this node failed while checking sandbox '{}': {}",
                    req.label, e
                ),
            ));
        }
    };
    tokio::fs::create_dir_all(&state.work_root)
        .await
        .map_err(|e| {
            err(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(
                    "create sandbox work root {} for '{}': {}",
                    state.work_root.display(),
                    req.label,
                    e
                ),
            )
        })?;
    let host_work_dir = work_dir_for(&state, &req.label);
    let created_work_dir = match tokio::fs::create_dir(&host_work_dir).await {
        Ok(()) => true,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && replacing => false,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(err(
                StatusCode::CONFLICT,
                format!(
                    "work directory {} for sandbox '{}' already exists without a container \
                     (left over from an interrupted create or destroy); destroy the sandbox to clean it up",
                    host_work_dir.display(),
                    req.label
                ),
            ));
        }
        Err(e) => {
            return Err(err(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(
                    "create work dir {} for sandbox '{}': {}",
                    host_work_dir.display(),
                    req.label,
                    e
                ),
            ));
        }
    };
    let config = SandboxCreateConfig {
        node_id: None,
        run_id: req.run_id,
        container_name_override: Some(req.label.clone()),
        host_work_dir: host_work_dir.clone(),
        workspace_volume: None,
        image: req.image,
        cpu_limit: req.cpu_limit,
        memory_limit_mb: req.memory_limit_mb,
        pids_limit: req.pids_limit,
        disk_size_mb: req.disk_size_mb,
        network_mode: req.network_mode,
        env_vars: req.env_vars,
        idle_timeout: Duration::from_secs(req.idle_timeout_secs),
        backend: req.backend,
        owner_user_id: None,
    };
    // The create runs in its own task: if the control plane gives up (its
    // create timeout, a dropped connection), this handler is cancelled but
    // the create still finishes and the guard in `run_create` cleans up.
    let (reply, result) = tokio::sync::oneshot::channel();
    tokio::spawn(run_create(
        host.provider.clone(),
        config,
        host_work_dir,
        created_work_dir,
        !replacing,
        claim,
        reply,
    ));
    match result.await {
        Ok(Ok(handle)) => {
            tracing::info!(
                sandbox = %handle.sandbox_name,
                replaced_stopped = replacing,
                "Created sandbox for control plane"
            );
            Ok(Json(handle))
        }
        Ok(Err(e)) => Err(provider_err(e)),
        Err(_) => Err(err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!(
                "create sandbox '{}': the create task ended without a result",
                req.label
            ),
        )),
    }
}

/// `POST /agent/sandboxes/exec`
#[utoipa::path(
    tag = "Sandboxes",
    post,
    path = "/agent/sandboxes/exec",
    request_body = RemoteExecRequest,
    responses(
        (status = 200, description = "Command finished (any exit code)", body = RemoteExecResponse),
        (status = 400, description = "Not a sandbox handle", body = RemoteErrorBody),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Sandbox container does not exist", body = RemoteErrorBody),
        (status = 500, description = "Exec failed", body = RemoteErrorBody),
        (status = 503, description = "No Docker daemon on this node", body = RemoteErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn exec_sandbox(
    State(state): HostState,
    Json(req): Json<RemoteExecRequest>,
) -> ApiResult<RemoteExecResponse> {
    let host = host(&state)?;
    let handle = require_handle(host, req.handle).await?;
    let provider = &host.provider;
    let result = if req.as_root {
        provider.exec_as_root(&handle, req.cmd, req.env, None).await
    } else if let Some(user) = req.user.as_deref() {
        provider
            .exec_as_user(&handle, user, req.cmd, req.env, None)
            .await
    } else {
        provider.exec(&handle, req.cmd, req.env, None).await
    }
    .map_err(provider_err)?;
    Ok(Json(RemoteExecResponse {
        exit_code: result.exit_code,
        stdout: keep_tail(result.stdout, WORKER_EXEC_OUTPUT_LIMIT),
        stderr: keep_tail(result.stderr, WORKER_EXEC_OUTPUT_LIMIT),
    }))
}

/// Frames buffered between a streamed exec and its HTTP response. When the
/// control plane reads slower than the command writes, the exec waits for
/// room (backpressure) instead of the worker buffering without bound.
const EXEC_STREAM_CHANNEL_FRAMES: usize = 64;

/// `POST /agent/sandboxes/exec-stream` — run a command and stream its
/// output as newline-delimited JSON [`RemoteExecFrame`]s while it runs.
///
/// The sandbox is resolved before the stream starts, so a bad handle or a
/// missing container still gets a plain error status. After that the
/// answer is `200` and ends with one `exit` or `error` frame; a heartbeat
/// frame is sent whenever the command stays quiet for
/// [`EXEC_STREAM_HEARTBEAT_INTERVAL`]. When the control plane disconnects
/// (its job was cancelled), the exec is dropped, exactly as a cancelled
/// local exec is; the process is stopped with `kill-processes`.
#[utoipa::path(
    tag = "Sandboxes",
    post,
    path = "/agent/sandboxes/exec-stream",
    request_body = RemoteExecRequest,
    responses(
        (status = 200, description = "Newline-delimited JSON exec frames, ending with an `exit` or `error` frame", body = RemoteExecFrame, content_type = "application/x-ndjson"),
        (status = 400, description = "Not a sandbox handle", body = RemoteErrorBody),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Sandbox container does not exist", body = RemoteErrorBody),
        (status = 503, description = "No Docker daemon on this node", body = RemoteErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn exec_sandbox_stream(
    State(state): HostState,
    Json(req): Json<RemoteExecRequest>,
) -> Result<Response, ApiError> {
    let host = host(&state)?;
    // The resolved handle replaces the request's; `run_streamed_exec`
    // ignores `req.handle`.
    let handle = require_handle(host, req.handle.clone()).await?;
    let (tx, rx) = tokio::sync::mpsc::channel(EXEC_STREAM_CHANNEL_FRAMES);
    let exec_id = uuid::Uuid::new_v4().simple().to_string();
    let finished = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let identity = if req.as_root {
        ExecIdentity::Root
    } else {
        match req.user.clone() {
            Some(user) => ExecIdentity::User(user),
            None => ExecIdentity::Default,
        }
    };
    let task = tokio::spawn(run_streamed_exec(
        host.provider.clone(),
        handle.clone(),
        req,
        exec_id.clone(),
        finished.clone(),
        state.work_root.clone(),
        tx,
    ));
    let guard = StreamedExecGuard {
        task,
        finished,
        provider: host.provider.clone(),
        handle,
        exec_id,
        identity,
    };
    Ok(exec_stream_response(
        rx,
        guard,
        EXEC_STREAM_HEARTBEAT_INTERVAL,
    ))
}

/// Environment variable carrying a streamed exec's id. Every process the
/// command starts inherits it, which is how [`StreamedExecGuard`] finds
/// them all to stop them if the control plane disconnects.
const STREAMED_EXEC_ID_ENV: &str = "TEMPS_SANDBOX_EXEC_ID";

/// How long processes get to exit after SIGTERM before they are killed.
const STREAMED_EXEC_KILL_GRACE_SECS: u32 = 5;

/// Run one exec, sending its output lines and then its outcome to `tx`.
/// `finished` is set once the command has exited (or failed), before the
/// last frame is sent.
async fn run_streamed_exec(
    provider: Arc<dyn SandboxProvider>,
    handle: SandboxHandle,
    req: RemoteExecRequest,
    exec_id: String,
    finished: Arc<std::sync::atomic::AtomicBool>,
    work_root: PathBuf,
    tx: tokio::sync::mpsc::Sender<RemoteExecFrame>,
) {
    let line_tx = tx.clone();
    let on_event: OnStreamEventCallback = Arc::new(move |stream: ExecStream, line: String| {
        let tx = line_tx.clone();
        Box::pin(async move {
            // A closed channel means the control plane went away; the
            // response drop aborts this task right after.
            for frame in RemoteExecFrame::output_frames(stream, line) {
                if tx.send(frame).await.is_err() {
                    break;
                }
            }
        })
    });
    let RemoteExecRequest {
        cmd,
        mut env,
        user,
        as_root,
        ..
    } = req;
    env.insert(STREAMED_EXEC_ID_ENV.to_string(), exec_id);
    // The provider's root/user variants take a stdout-only callback, as
    // they do on the control plane.
    let stdout_cb = || -> OnEventCallback {
        let on_event = on_event.clone();
        Arc::new(move |line: String| on_event(ExecStream::Stdout, line))
    };
    let result = if as_root {
        provider
            .exec_as_root(&handle, cmd, env, Some(stdout_cb()))
            .await
    } else if let Some(user) = user.as_deref() {
        provider
            .exec_as_user(&handle, user, cmd, env, Some(stdout_cb()))
            .await
    } else {
        provider
            .exec_streamed(&handle, cmd, env, Some(on_event.clone()))
            .await
    };
    finished.store(true, std::sync::atomic::Ordering::SeqCst);
    let frame = match result {
        Ok(result) => RemoteExecFrame::Exit {
            exit_code: result.exit_code,
        },
        Err(e) => {
            let (status, Json(body)) = provider_err(e);
            // The redaction middleware only sees error *responses*; this
            // error travels inside a 200 stream, so redact it here.
            let error = match redact_work_root(&body.error, &work_root) {
                Some(redacted) => {
                    tracing::warn!(
                        sandbox = %handle.sandbox_name,
                        status = status.as_u16(),
                        error = %body.error,
                        "Streamed sandbox exec failed"
                    );
                    redacted
                }
                None => body.error,
            };
            RemoteExecFrame::Error {
                status: status.as_u16(),
                error,
            }
        }
    };
    let _ = tx.send(frame).await;
}

/// Owns a streamed exec for as long as its response body lives. When the
/// body is dropped before the command finished — the control plane
/// disconnected, or its job was cancelled — it aborts the task waiting on
/// Docker *and* stops the command's processes inside the sandbox: aborting
/// the task alone would leave them running with nobody reading their output.
struct StreamedExecGuard {
    task: tokio::task::JoinHandle<()>,
    finished: Arc<std::sync::atomic::AtomicBool>,
    provider: Arc<dyn SandboxProvider>,
    handle: SandboxHandle,
    exec_id: String,
    /// Who the command ran as. Its processes are stopped as the same user:
    /// sandboxes drop every capability, so even root cannot read another
    /// user's `/proc/<pid>/environ`, but a user can always read its own.
    identity: ExecIdentity,
}

/// The user a streamed exec ran as.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ExecIdentity {
    /// The sandbox's default user.
    Default,
    Root,
    User(String),
}

impl Drop for StreamedExecGuard {
    fn drop(&mut self) {
        self.task.abort();
        if self.finished.load(std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            tracing::error!(
                sandbox = %self.handle.sandbox_name,
                exec_id = %self.exec_id,
                "No runtime to stop a disconnected streamed exec; its processes keep running"
            );
            return;
        };
        let provider = self.provider.clone();
        let handle = self.handle.clone();
        let exec_id = self.exec_id.clone();
        let identity = self.identity.clone();
        runtime.spawn(async move {
            tracing::info!(
                sandbox = %handle.sandbox_name,
                exec_id = %exec_id,
                identity = ?identity,
                "Control plane disconnected from a running streamed exec; stopping its processes"
            );
            let cmd = stop_streamed_exec_cmd(&exec_id);
            let env = std::collections::HashMap::new();
            let stopped = match &identity {
                ExecIdentity::Default => provider.exec(&handle, cmd, env, None).await,
                ExecIdentity::Root => provider.exec_as_root(&handle, cmd, env, None).await,
                ExecIdentity::User(user) => {
                    provider.exec_as_user(&handle, user, cmd, env, None).await
                }
            };
            match stopped {
                Ok(result) if result.exit_code == 0 => {}
                Ok(result) => tracing::warn!(
                    sandbox = %handle.sandbox_name,
                    exec_id = %exec_id,
                    exit_code = result.exit_code,
                    stderr = %result.stderr,
                    "Stopping a disconnected streamed exec's processes failed"
                ),
                Err(e) => tracing::warn!(
                    sandbox = %handle.sandbox_name,
                    exec_id = %exec_id,
                    error = %e,
                    "Could not stop a disconnected streamed exec's processes"
                ),
            }
        });
    }
}

/// Command (run in the sandbox as the exec's own user) that stops every
/// process carrying `exec_id` in [`STREAMED_EXEC_ID_ENV`]: SIGTERM, then
/// SIGKILL for whatever is left after [`STREAMED_EXEC_KILL_GRACE_SECS`].
/// Processes whose environment is unreadable (another user's, or already
/// gone) are skipped quietly. The id is passed as an argument, never
/// interpolated into the script.
fn stop_streamed_exec_cmd(exec_id: &str) -> Vec<String> {
    let script = format!(
        r#"pids() {{ for p in /proc/[0-9]*; do {{ tr '\0' '\n' < "$p/environ"; }} 2>/dev/null | grep -qx "{env}=$1" && echo "${{p#/proc/}}"; done; }}
found=$(pids "$1")
[ -z "$found" ] && exit 0
kill -TERM $found 2>/dev/null
sleep {grace}
found=$(pids "$1")
[ -n "$found" ] && kill -KILL $found 2>/dev/null
exit 0"#,
        env = STREAMED_EXEC_ID_ENV,
        grace = STREAMED_EXEC_KILL_GRACE_SECS,
    );
    vec![
        "sh".to_string(),
        "-c".to_string(),
        script,
        "sh".to_string(),
        exec_id.to_string(),
    ]
}

/// One NDJSON line for `frame`.
fn encode_exec_frame(frame: &RemoteExecFrame) -> bytes::Bytes {
    match serde_json::to_vec(frame) {
        Ok(mut line) => {
            line.push(b'\n');
            line.into()
        }
        Err(e) => {
            tracing::error!(error = %e, "Could not encode a sandbox exec frame");
            bytes::Bytes::from_static(
                b"{\"type\":\"error\",\"status\":500,\"error\":\"the worker node could not encode an exec frame\"}\n",
            )
        }
    }
}

/// The streaming response: frames from `rx` as they come, a heartbeat after
/// every `heartbeat` of silence, and the end of the body once the exec task
/// has sent its last frame. The body owns `task` (a [`StreamedExecGuard`] in
/// production), so dropping the body drops it.
fn exec_stream_response<G: Send + 'static>(
    rx: tokio::sync::mpsc::Receiver<RemoteExecFrame>,
    task: G,
    heartbeat: Duration,
) -> Response {
    let body = futures::stream::unfold((rx, task), move |(mut rx, task)| async move {
        let frame = tokio::select! {
            frame = rx.recv() => frame?,
            _ = tokio::time::sleep(heartbeat) => RemoteExecFrame::Heartbeat,
        };
        Some((
            Ok::<_, std::convert::Infallible>(encode_exec_frame(&frame)),
            (rx, task),
        ))
    });
    (
        [(axum::http::header::CONTENT_TYPE, "application/x-ndjson")],
        axum::body::Body::from_stream(body),
    )
        .into_response()
}

/// Keep the last `limit` bytes of `output` (where errors usually are),
/// marking how much was dropped.
fn keep_tail(output: String, limit: usize) -> String {
    if output.len() <= limit {
        return output;
    }
    let mut start = output.len() - limit;
    while !output.is_char_boundary(start) {
        start += 1;
    }
    format!(
        "[{} earlier bytes truncated by the worker node]\n{}",
        start,
        &output[start..]
    )
}

/// `POST /agent/sandboxes/alive`
#[utoipa::path(
    tag = "Sandboxes",
    post,
    path = "/agent/sandboxes/alive",
    request_body = RemoteHandleRequest,
    responses(
        (status = 200, description = "Whether the sandbox is running (false when it does not exist)", body = RemoteAliveResponse),
        (status = 400, description = "Not a sandbox handle", body = RemoteErrorBody),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Liveness check failed", body = RemoteErrorBody),
        (status = 503, description = "No Docker daemon on this node", body = RemoteErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn sandbox_alive(
    State(state): HostState,
    Json(req): Json<RemoteHandleRequest>,
) -> ApiResult<RemoteAliveResponse> {
    let host = host(&state)?;
    let alive = match resolve_handle(host, req.handle).await? {
        Some(handle) => host
            .provider
            .is_alive(&handle)
            .await
            .map_err(provider_err)?,
        None => false,
    };
    Ok(Json(RemoteAliveResponse { alive }))
}

/// `POST /agent/sandboxes/read-file` — at most
/// [`WORKER_READ_FILE_MAX_BYTES`]; bigger files are refused with a 400
/// before they are buffered.
#[utoipa::path(
    tag = "Sandboxes",
    post,
    path = "/agent/sandboxes/read-file",
    request_body = RemoteReadFileRequest,
    responses(
        (status = 200, description = "File contents", body = RemoteFileContents),
        (status = 400, description = "Not a sandbox handle, or the file is over the size limit", body = RemoteErrorBody),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Sandbox container does not exist", body = RemoteErrorBody),
        (status = 500, description = "Read failed", body = RemoteErrorBody),
        (status = 503, description = "No Docker daemon on this node", body = RemoteErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn read_sandbox_file(
    State(state): HostState,
    Json(req): Json<RemoteReadFileRequest>,
) -> ApiResult<RemoteFileContents> {
    let host = host(&state)?;
    let handle = require_handle(host, req.handle).await?;
    let bytes = host
        .provider
        .read_file_bounded(&handle, &req.path, WORKER_READ_FILE_MAX_BYTES)
        .await
        .map_err(provider_err)?;
    Ok(Json(RemoteFileContents {
        contents_b64: base64::engine::general_purpose::STANDARD.encode(bytes),
    }))
}

/// `POST /agent/sandboxes/write-file`
#[utoipa::path(
    tag = "Sandboxes",
    post,
    path = "/agent/sandboxes/write-file",
    request_body = RemoteWriteFileRequest,
    responses(
        (status = 200, description = "File written", body = RemoteOkResponse),
        (status = 400, description = "Not a sandbox handle, or invalid base64", body = RemoteErrorBody),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Sandbox container does not exist", body = RemoteErrorBody),
        (status = 413, description = "Body over the upload limit"),
        (status = 500, description = "Write failed", body = RemoteErrorBody),
        (status = 503, description = "No Docker daemon, or every upload slot is busy", body = RemoteErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn write_sandbox_file(
    State(state): HostState,
    Json(req): Json<RemoteWriteFileRequest>,
) -> ApiResult<RemoteOkResponse> {
    let host = host(&state)?;
    let handle = require_handle(host, req.handle).await?;
    let contents = base64::engine::general_purpose::STANDARD
        .decode(req.contents_b64)
        .map_err(|e| {
            err(
                StatusCode::BAD_REQUEST,
                format!(
                    "write-file '{}' in sandbox {}: invalid base64: {}",
                    req.path, handle.sandbox_name, e
                ),
            )
        })?;
    host.provider
        .write_file(&handle, &req.path, &contents, req.mode)
        .await
        .map_err(provider_err)?;
    Ok(ok())
}

/// Why an uploaded directory archive was not unpacked.
#[derive(Debug)]
enum UnpackError {
    /// The archive is not acceptable (bad entry type or path, too big).
    Rejected(String),
    /// Unpacking an acceptable entry failed on this node.
    Failed(String),
}

/// Unpack an uploaded directory archive into `dest`, an empty directory.
///
/// Only regular files and directories with relative paths are accepted:
/// symlinks and hard links (which `write_directory` would follow to read
/// host files), device nodes, fifos and sparse files are refused with the
/// entry named, as are absolute paths and `..`. Each entry is unpacked with
/// `unpack_in`, which re-checks that it stays under `dest`. Owner, mode
/// bits, mtimes and xattrs from the archive are never applied to the host.
fn unpack_upload(archive: &[u8], dest: &Path) -> Result<(), UnpackError> {
    let mut tar = tar::Archive::new(archive);
    tar.set_preserve_permissions(false);
    tar.set_preserve_ownerships(false);
    tar.set_preserve_mtime(false);
    tar.set_unpack_xattrs(false);
    let entries = tar
        .entries()
        .map_err(|e| UnpackError::Rejected(format!("not a tar archive: {}", e)))?;
    let mut unpacked_bytes: u64 = 0;
    for (index, entry) in entries.enumerate() {
        if index >= MAX_UPLOAD_ENTRIES {
            return Err(UnpackError::Rejected(format!(
                "more than {} entries",
                MAX_UPLOAD_ENTRIES
            )));
        }
        let mut entry = entry
            .map_err(|e| UnpackError::Rejected(format!("entry #{} is malformed: {}", index, e)))?;
        let path = entry
            .path()
            .map_err(|e| {
                UnpackError::Rejected(format!("entry #{} has an invalid path: {}", index, e))
            })?
            .into_owned();
        let shown = path.display().to_string();
        let kind = entry.header().entry_type();
        if !(kind.is_file() || kind.is_dir()) {
            return Err(UnpackError::Rejected(format!(
                "entry '{}' is a {:?}; only regular files and directories are accepted",
                shown, kind
            )));
        }
        for component in path.components() {
            match component {
                Component::RootDir | Component::Prefix(_) => {
                    return Err(UnpackError::Rejected(format!(
                        "entry '{}' has an absolute path",
                        shown
                    )));
                }
                Component::ParentDir => {
                    return Err(UnpackError::Rejected(format!(
                        "entry '{}' contains '..'",
                        shown
                    )));
                }
                Component::CurDir | Component::Normal(_) => {}
            }
        }
        unpacked_bytes = unpacked_bytes.saturating_add(entry.size());
        if unpacked_bytes > MAX_UPLOAD_UNPACKED_BYTES {
            return Err(UnpackError::Rejected(format!(
                "entry '{}' takes the archive over the {} byte unpacked limit",
                shown, MAX_UPLOAD_UNPACKED_BYTES
            )));
        }
        match entry.unpack_in(dest) {
            Ok(true) => {}
            Ok(false) => {
                return Err(UnpackError::Rejected(format!(
                    "entry '{}' would land outside the upload directory",
                    shown
                )));
            }
            Err(e) => {
                return Err(UnpackError::Failed(format!(
                    "unpack entry '{}' into {}: {}",
                    shown,
                    dest.display(),
                    e
                )));
            }
        }
    }
    Ok(())
}

/// `POST /agent/sandboxes/write-directory`
#[utoipa::path(
    tag = "Sandboxes",
    post,
    path = "/agent/sandboxes/write-directory",
    request_body = RemoteWriteDirectoryRequest,
    responses(
        (status = 200, description = "Directory written", body = RemoteOkResponse),
        (status = 400, description = "Not a sandbox handle, invalid base64, or an archive entry that is not a regular file or directory with a relative path", body = RemoteErrorBody),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Sandbox container does not exist", body = RemoteErrorBody),
        (status = 413, description = "Body over the upload limit"),
        (status = 500, description = "Write failed", body = RemoteErrorBody),
        (status = 503, description = "No Docker daemon, or every upload slot is busy", body = RemoteErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn write_sandbox_directory(
    State(state): HostState,
    Json(req): Json<RemoteWriteDirectoryRequest>,
) -> ApiResult<RemoteOkResponse> {
    let host = host(&state)?;
    let handle = require_handle(host, req.handle).await?;
    let sandbox = handle.sandbox_name.clone();
    let archive = base64::engine::general_purpose::STANDARD
        .decode(req.tar_b64)
        .map_err(|e| {
            err(
                StatusCode::BAD_REQUEST,
                format!(
                    "write-directory to '{}' in sandbox {}: invalid base64: {}",
                    req.target_path, sandbox, e
                ),
            )
        })?;
    tokio::fs::create_dir_all(&state.work_root)
        .await
        .map_err(|e| {
            err(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(
                    "write-directory for sandbox {}: create work root {}: {}",
                    sandbox,
                    state.work_root.display(),
                    e
                ),
            )
        })?;
    let staging = tempfile::Builder::new()
        .prefix(".incoming-")
        .tempdir_in(&state.work_root)
        .map_err(|e| {
            err(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(
                    "write-directory for sandbox {}: create staging dir in {}: {}",
                    sandbox,
                    state.work_root.display(),
                    e
                ),
            )
        })?;
    // `staging` is a fresh, empty directory, and `unpack_upload` accepts
    // only regular files and directories with relative paths, so nothing in
    // staging can point outside it when the provider reads it back.
    let dest = staging.path().to_path_buf();
    tokio::task::spawn_blocking(move || unpack_upload(&archive, &dest))
        .await
        .map_err(|e| {
            err(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(
                    "write-directory for sandbox {}: unpack task failed: {}",
                    sandbox, e
                ),
            )
        })?
        .map_err(|e| match e {
            UnpackError::Rejected(reason) => err(
                StatusCode::BAD_REQUEST,
                format!(
                    "write-directory to '{}' in sandbox {}: archive refused: {}",
                    req.target_path, sandbox, reason
                ),
            ),
            UnpackError::Failed(reason) => err(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(
                    "write-directory to '{}' in sandbox {}: {}",
                    req.target_path, sandbox, reason
                ),
            ),
        })?;
    host.provider
        .write_directory(&handle, staging.path(), &req.target_path)
        .await
        .map_err(provider_err)?;
    Ok(ok())
}

/// `POST /agent/sandboxes/kill-processes`
#[utoipa::path(
    tag = "Sandboxes",
    post,
    path = "/agent/sandboxes/kill-processes",
    request_body = RemoteKillRequest,
    responses(
        (status = 200, description = "Signal sent", body = RemoteOkResponse),
        (status = 400, description = "Not a sandbox handle", body = RemoteErrorBody),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Sandbox container does not exist", body = RemoteErrorBody),
        (status = 500, description = "Kill failed", body = RemoteErrorBody),
        (status = 503, description = "No Docker daemon on this node", body = RemoteErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn kill_sandbox_processes(
    State(state): HostState,
    Json(req): Json<RemoteKillRequest>,
) -> ApiResult<RemoteOkResponse> {
    let host = host(&state)?;
    let handle = require_handle(host, req.handle).await?;
    host.provider
        .kill_processes(&handle, &req.pattern, req.signal)
        .await
        .map_err(provider_err)?;
    Ok(ok())
}

/// `POST /agent/sandboxes/destroy` — removes the container and this node's
/// work directory for it.
#[utoipa::path(
    tag = "Sandboxes",
    post,
    path = "/agent/sandboxes/destroy",
    request_body = RemoteDestroyRequest,
    responses(
        (status = 200, description = "Destroyed (or already gone)", body = RemoteOkResponse),
        (status = 400, description = "Not a sandbox handle", body = RemoteErrorBody),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Destroy failed", body = RemoteErrorBody),
        (status = 503, description = "No Docker daemon on this node", body = RemoteErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn destroy_sandbox(
    State(state): HostState,
    Json(req): Json<RemoteDestroyRequest>,
) -> ApiResult<RemoteOkResponse> {
    let host = host(&state)?;
    let label = label_of(&req.handle).map(str::to_string);
    // Already gone is fine: destroy is idempotent, and the work dir below is
    // still cleaned up.
    if let Some(handle) = resolve_handle(host, req.handle).await? {
        host.provider
            .destroy(&handle, req.purge_volumes)
            .await
            .map_err(provider_err)?;
    }
    if let Some(label) = label.as_deref() {
        remove_work_dir(&work_dir_for(&state, label)).await;
    }
    Ok(ok())
}

/// `POST /agent/sandboxes/stop`
#[utoipa::path(
    tag = "Sandboxes",
    post,
    path = "/agent/sandboxes/stop",
    request_body = RemoteHandleRequest,
    responses(
        (status = 200, description = "Stopped", body = RemoteOkResponse),
        (status = 400, description = "Not a sandbox handle", body = RemoteErrorBody),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Sandbox container does not exist", body = RemoteErrorBody),
        (status = 500, description = "Stop failed", body = RemoteErrorBody),
        (status = 503, description = "No Docker daemon on this node", body = RemoteErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn stop_sandbox(
    State(state): HostState,
    Json(req): Json<RemoteHandleRequest>,
) -> ApiResult<RemoteOkResponse> {
    let host = host(&state)?;
    let handle = require_handle(host, req.handle).await?;
    host.provider.stop(&handle).await.map_err(provider_err)?;
    Ok(ok())
}

/// `POST /agent/sandboxes/start`
#[utoipa::path(
    tag = "Sandboxes",
    post,
    path = "/agent/sandboxes/start",
    request_body = RemoteHandleRequest,
    responses(
        (status = 200, description = "Started", body = RemoteOkResponse),
        (status = 400, description = "Not a sandbox handle", body = RemoteErrorBody),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Sandbox container does not exist", body = RemoteErrorBody),
        (status = 500, description = "Start failed", body = RemoteErrorBody),
        (status = 503, description = "No Docker daemon on this node", body = RemoteErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn start_sandbox(
    State(state): HostState,
    Json(req): Json<RemoteHandleRequest>,
) -> ApiResult<RemoteOkResponse> {
    let host = host(&state)?;
    let handle = require_handle(host, req.handle).await?;
    host.provider.start(&handle).await.map_err(provider_err)?;
    Ok(ok())
}

/// `POST /agent/sandboxes/recover` — find a sandbox container by name after
/// a control-plane restart.
#[utoipa::path(
    tag = "Sandboxes",
    post,
    path = "/agent/sandboxes/recover",
    request_body = RemoteRecoverRequest,
    responses(
        (status = 200, description = "The recovered handle, or null when there is no such sandbox", body = Option<SandboxHandle>),
        (status = 400, description = "Invalid sandbox label", body = RemoteErrorBody),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Recovery failed", body = RemoteErrorBody),
        (status = 503, description = "No Docker daemon on this node", body = RemoteErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn recover_sandbox(
    State(state): HostState,
    Json(req): Json<RemoteRecoverRequest>,
) -> ApiResult<Option<SandboxHandle>> {
    let host = host(&state)?;
    // The wire carries the bare label; the provider adds the
    // `temps-sandbox-` prefix itself, so recovery can never reach the node's
    // deployment or service containers.
    if !is_valid_sandbox_label(&req.container_name) {
        return Err(err(
            StatusCode::BAD_REQUEST,
            format!("invalid sandbox name '{}'", req.container_name),
        ));
    }
    let container_name = format!("{}{}", SANDBOX_CONTAINER_PREFIX, req.container_name);
    match host.containers.find(&container_name).await {
        Ok(NamedContainer::Sandbox { .. }) => {}
        Ok(NamedContainer::Missing) => return Ok(Json(None)),
        Ok(NamedContainer::Other) => {
            return Err(err(
                StatusCode::BAD_REQUEST,
                format!(
                    "container '{}' on this node is not a sandbox (no {} label)",
                    container_name, SANDBOX_CONTAINER_LABEL
                ),
            ));
        }
        Err(e) => {
            return Err(err(
                StatusCode::SERVICE_UNAVAILABLE,
                format!(
                    "the Docker daemon on this node failed while recovering sandbox '{}': {}",
                    req.container_name, e
                ),
            ));
        }
    }
    let handle = host
        .provider
        .recover_by_name(&req.container_name)
        .await
        .map_err(provider_err)?;
    Ok(Json(handle))
}

/// `POST /agent/sandboxes/status` — whether this node can run sandboxes.
#[utoipa::path(
    tag = "Sandboxes",
    post,
    path = "/agent/sandboxes/status",
    responses(
        (status = 200, description = "Sandbox availability on this node", body = RemoteStatusResponse),
        (status = 401, description = "Unauthorized")
    ),
    security(("bearer_auth" = []))
)]
pub async fn sandbox_status(State(state): HostState) -> ApiResult<RemoteStatusResponse> {
    let Some(SandboxHost { provider, .. }) = state.host.as_ref() else {
        return Ok(Json(RemoteStatusResponse {
            available: false,
            image_ready: false,
            image: String::new(),
        }));
    };
    let available = provider.is_available().await;
    let (image_ready, image) = match provider.image_status().await {
        Ok(status) => status,
        Err(e) => {
            tracing::warn!(error = %e, "Sandbox image status check failed");
            (false, String::new())
        }
    };
    Ok(Json(RemoteStatusResponse {
        available,
        image_ready,
        image,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;
    use temps_agents::sandbox::{KillSignal, SandboxBackend, SandboxExecResult};
    use tokio::sync::Notify;

    #[test]
    fn redaction_replaces_the_work_root_everywhere_in_a_message() {
        let root = Path::new("/var/lib/temps/sandboxes");
        let redacted = redact_work_root(
            "create work dir /var/lib/temps/sandboxes/abc for 'abc': exists; \
             staging /var/lib/temps/sandboxes/.incoming-x failed",
            root,
        )
        .expect("redacted");
        assert!(!redacted.contains("/var/lib/temps"), "{redacted}");
        assert_eq!(
            redacted.matches(REDACTED_WORK_ROOT).count(),
            2,
            "{redacted}"
        );
        assert!(redacted.contains("'abc'"), "the label stays: {redacted}");
    }

    #[test]
    fn redaction_leaves_messages_without_the_work_root_alone() {
        let root = Path::new("/var/lib/temps/sandboxes/");
        assert_eq!(redact_work_root("sandbox 'abc' is not running", root), None);
        assert_eq!(redact_work_root("anything", Path::new("")), None);
    }

    /// Error bodies leaving the worker never carry its filesystem layout;
    /// successful responses pass through untouched.
    #[tokio::test]
    async fn sandbox_error_bodies_are_redacted_on_the_way_out() {
        use tower::ServiceExt;
        let root = tempfile::tempdir().expect("tempdir");
        let work_root = root.path().join("sandboxes");
        let state = Arc::new(SandboxHostState::new(None, work_root.clone()));
        let failing_path = work_root.join("abc");
        let app = axum::Router::new()
            .route(
                "/fail",
                axum::routing::post(move || {
                    let failing_path = failing_path.clone();
                    async move {
                        err(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            format!(
                                "create work dir {} for 'abc': denied",
                                failing_path.display()
                            ),
                        )
                    }
                }),
            )
            .route(
                "/ok",
                axum::routing::post(|| async { Json(RemoteOkResponse { ok: true }) }),
            )
            .layer(axum::middleware::from_fn_with_state(
                state.clone(),
                redact_host_paths,
            ))
            .with_state(state);

        let response = app
            .clone()
            .oneshot(
                axum::http::Request::post("/fail")
                    .body(axum::body::Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("body");
        let body: RemoteErrorBody = serde_json::from_slice(&body).expect("still an error body");
        assert!(
            !body.error.contains(&*work_root.to_string_lossy()),
            "{}",
            body.error
        );
        assert!(body.error.contains(REDACTED_WORK_ROOT), "{}", body.error);
        assert!(body.error.contains("'abc'"), "{}", body.error);

        let response = app
            .oneshot(
                axum::http::Request::post("/ok")
                    .body(axum::body::Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
    }

    /// Command and environment of one recorded exec.
    type ExecCall = (Vec<String>, HashMap<String, String>);

    /// Records every call; `create` can be held until released.
    #[derive(Default)]
    struct FakeProvider {
        seen_ids: Mutex<Vec<String>>,
        destroyed: Mutex<Vec<(String, bool)>>,
        /// Files `write_directory` found in the staging dir, by relative path.
        uploaded: Mutex<Vec<(String, Vec<u8>)>>,
        read_limits: Mutex<Vec<u64>>,
        create_gate: Option<Arc<Notify>>,
        create_entered: Arc<Notify>,
        create_fails: bool,
        /// Lines an exec produces (stdout-only callbacks get stdout lines).
        exec_lines: Vec<(ExecStream, String)>,
        /// Fail every exec with this reason instead of exiting.
        exec_fails_with: Option<String>,
        /// `exec_streamed` never returns (a dev server).
        exec_hangs: bool,
        /// Command and environment of every `exec` / `exec_streamed` call
        /// (`exec_as_root` reaches `exec` through the trait default).
        exec_calls: Mutex<Vec<ExecCall>>,
    }

    impl FakeProvider {
        fn saw(&self, handle: &SandboxHandle) {
            self.seen_ids
                .lock()
                .unwrap()
                .push(handle.sandbox_id.clone());
        }
    }

    #[async_trait::async_trait]
    impl SandboxProvider for FakeProvider {
        async fn create(&self, config: SandboxCreateConfig) -> Result<SandboxHandle, AgentError> {
            self.create_entered.notify_one();
            if let Some(gate) = &self.create_gate {
                gate.notified().await;
            }
            if self.create_fails {
                return Err(AgentError::SandboxCreationFailed {
                    run_id: config.run_id,
                    provider: "fake".into(),
                    reason: "image pull failed".into(),
                });
            }
            let name = format!(
                "{}{}",
                SANDBOX_CONTAINER_PREFIX,
                config.container_name_override.unwrap_or_default()
            );
            Ok(sandbox_handle(&name, "created-id"))
        }
        async fn exec(
            &self,
            handle: &SandboxHandle,
            cmd: Vec<String>,
            env: HashMap<String, String>,
            on_output: Option<OnEventCallback>,
        ) -> Result<SandboxExecResult, AgentError> {
            self.saw(handle);
            self.exec_calls.lock().unwrap().push((cmd, env));
            if let Some(cb) = on_output {
                for (stream, line) in &self.exec_lines {
                    if *stream == ExecStream::Stdout {
                        cb(line.clone()).await;
                    }
                }
            }
            Ok(SandboxExecResult {
                exit_code: 0,
                stdout: "ok".into(),
                stderr: String::new(),
            })
        }
        async fn exec_streamed(
            &self,
            handle: &SandboxHandle,
            cmd: Vec<String>,
            env: HashMap<String, String>,
            on_event: Option<OnStreamEventCallback>,
        ) -> Result<SandboxExecResult, AgentError> {
            self.saw(handle);
            self.exec_calls.lock().unwrap().push((cmd, env));
            if let Some(cb) = on_event {
                for (stream, line) in &self.exec_lines {
                    cb(*stream, line.clone()).await;
                }
            }
            if self.exec_hangs {
                std::future::pending::<()>().await;
            }
            match &self.exec_fails_with {
                Some(reason) => Err(AgentError::SandboxExecFailed {
                    run_id: 0,
                    sandbox_id: handle.sandbox_name.clone(),
                    reason: reason.clone(),
                }),
                None => Ok(SandboxExecResult {
                    exit_code: 7,
                    stdout: String::new(),
                    stderr: String::new(),
                }),
            }
        }
        async fn is_alive(&self, handle: &SandboxHandle) -> Result<bool, AgentError> {
            self.saw(handle);
            Ok(true)
        }
        async fn write_file(
            &self,
            handle: &SandboxHandle,
            _path: &str,
            _contents: &[u8],
            _mode: u32,
        ) -> Result<(), AgentError> {
            self.saw(handle);
            Ok(())
        }
        async fn read_file(
            &self,
            handle: &SandboxHandle,
            _path: &str,
        ) -> Result<Vec<u8>, AgentError> {
            self.saw(handle);
            Ok(b"data".to_vec())
        }
        async fn read_file_bounded(
            &self,
            handle: &SandboxHandle,
            path: &str,
            max_bytes: u64,
        ) -> Result<Vec<u8>, AgentError> {
            self.read_limits.lock().unwrap().push(max_bytes);
            if path == "/huge" {
                return Err(AgentError::Validation {
                    message: format!("read_file: '{path}' is over {max_bytes} bytes"),
                });
            }
            self.read_file(handle, path).await
        }
        async fn write_directory(
            &self,
            handle: &SandboxHandle,
            local_dir: &std::path::Path,
            _target_path: &str,
        ) -> Result<(), AgentError> {
            self.saw(handle);
            let mut files = Vec::new();
            collect_tree(local_dir, local_dir, &mut files);
            *self.uploaded.lock().unwrap() = files;
            Ok(())
        }
        async fn kill_processes(
            &self,
            handle: &SandboxHandle,
            _pattern: &str,
            _signal: KillSignal,
        ) -> Result<(), AgentError> {
            self.saw(handle);
            Ok(())
        }
        async fn destroy(&self, handle: &SandboxHandle, purge: bool) -> Result<(), AgentError> {
            self.saw(handle);
            self.destroyed
                .lock()
                .unwrap()
                .push((handle.sandbox_name.clone(), purge));
            Ok(())
        }
        async fn stop(&self, handle: &SandboxHandle) -> Result<(), AgentError> {
            self.saw(handle);
            Ok(())
        }
        async fn start(&self, handle: &SandboxHandle) -> Result<(), AgentError> {
            self.saw(handle);
            Ok(())
        }
        async fn restart(&self, handle: &SandboxHandle) -> Result<(), AgentError> {
            self.saw(handle);
            Ok(())
        }
        async fn recover(&self, _run_id: i32) -> Result<Option<SandboxHandle>, AgentError> {
            Ok(None)
        }
        async fn recover_by_name(&self, name: &str) -> Result<Option<SandboxHandle>, AgentError> {
            Ok(Some(sandbox_handle(
                &format!("{SANDBOX_CONTAINER_PREFIX}{name}"),
                "recovered-id",
            )))
        }
        fn name(&self) -> &str {
            "fake"
        }
        async fn is_available(&self) -> bool {
            true
        }
        async fn image_status(&self) -> Result<(bool, String), AgentError> {
            Ok((true, "fake:latest".into()))
        }
        async fn rebuild_image(&self) -> Result<String, AgentError> {
            Ok("fake:latest".into())
        }
    }

    /// Every regular file under `dir`, as (path relative to `root`, bytes),
    /// sorted. Symlinks are reported as such so a test sees them.
    fn collect_tree(root: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        entries.sort();
        for path in entries {
            let meta = std::fs::symlink_metadata(&path).unwrap();
            let rel = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            if meta.file_type().is_symlink() {
                out.push((format!("{rel} (symlink)"), Vec::new()));
            } else if meta.is_dir() {
                collect_tree(root, &path, out);
            } else {
                out.push((rel, std::fs::read(&path).unwrap()));
            }
        }
    }

    /// Containers on the fake node, by exact name.
    struct FakeContainers(HashMap<String, NamedContainer>);

    #[async_trait::async_trait]
    impl ContainerLookup for FakeContainers {
        async fn find(&self, name: &str) -> Result<NamedContainer, String> {
            Ok(self.0.get(name).cloned().unwrap_or(NamedContainer::Missing))
        }
    }

    fn sandbox_handle(name: &str, id: &str) -> SandboxHandle {
        SandboxHandle {
            node_id: None,
            sandbox_id: id.into(),
            sandbox_name: name.into(),
            work_dir: PathBuf::from("/home/temps/workspace"),
            backend: SandboxBackend::Docker,
            image: String::new(),
        }
    }

    struct Node {
        state: Arc<SandboxHostState>,
        provider: Arc<FakeProvider>,
        _root: tempfile::TempDir,
    }

    fn running(id: &str) -> NamedContainer {
        NamedContainer::Sandbox {
            id: id.into(),
            running: true,
        }
    }

    /// A node hosting one sandbox, `temps-sandbox-abc` (real container id
    /// `real-id`), plus `extra` containers.
    fn node_with(provider: FakeProvider, extra: Vec<(&str, NamedContainer)>) -> Node {
        let root = tempfile::tempdir().unwrap();
        let provider = Arc::new(provider);
        let mut containers = HashMap::from([("temps-sandbox-abc".to_string(), running("real-id"))]);
        for (name, container) in extra {
            containers.insert(name.to_string(), container);
        }
        let state = Arc::new(SandboxHostState::new(
            Some(SandboxHost {
                provider: provider.clone(),
                containers: Arc::new(FakeContainers(containers)),
            }),
            root.path().to_path_buf(),
        ));
        Node {
            state,
            provider,
            _root: root,
        }
    }

    fn node() -> Node {
        node_with(FakeProvider::default(), Vec::new())
    }

    fn exec_req(handle: SandboxHandle) -> Json<RemoteExecRequest> {
        Json(RemoteExecRequest {
            handle,
            cmd: vec!["true".into()],
            env: HashMap::new(),
            user: None,
            as_root: false,
        })
    }

    fn create_req(label: &str) -> Json<RemoteCreateRequest> {
        Json(RemoteCreateRequest {
            run_id: 1,
            label: label.into(),
            image: None,
            cpu_limit: None,
            memory_limit_mb: None,
            pids_limit: None,
            disk_size_mb: None,
            network_mode: None,
            env_vars: HashMap::new(),
            idle_timeout_secs: 60,
            backend: None,
        })
    }

    #[tokio::test]
    async fn operations_use_the_container_docker_resolves_not_the_request_id() {
        let n = node();
        // A valid sandbox name smuggling an application container's id.
        let handle = sandbox_handle("temps-sandbox-abc", "app-container-id");
        let _ = exec_sandbox(State(n.state.clone()), exec_req(handle))
            .await
            .unwrap();
        assert_eq!(*n.provider.seen_ids.lock().unwrap(), vec!["real-id"]);
    }

    #[tokio::test]
    async fn non_sandbox_handles_are_refused() {
        let n = node();
        for name in [
            "my-app-web-1",
            // The egress proxy sidecar shares the name prefix.
            "temps-sandbox-egress-proxy-v2-temps-sandbox-abc",
        ] {
            let (status, _) = exec_sandbox(
                State(n.state.clone()),
                exec_req(sandbox_handle(name, "real-id")),
            )
            .await
            .unwrap_err();
            assert_eq!(status, StatusCode::BAD_REQUEST, "{name}");
        }
        assert!(n.provider.seen_ids.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn prefixed_containers_without_the_sandbox_label_are_refused() {
        // e.g. an app container of a project whose slug starts with
        // `temps-sandbox`.
        let n = node_with(
            FakeProvider::default(),
            vec![("temps-sandbox-site-web-1", NamedContainer::Other)],
        );
        let impostor = sandbox_handle("temps-sandbox-site-web-1", "x");
        let (status, Json(body)) = exec_sandbox(State(n.state.clone()), exec_req(impostor.clone()))
            .await
            .unwrap_err();
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body.error.contains("not a sandbox"), "{}", body.error);
        let (status, _) = sandbox_alive(
            State(n.state.clone()),
            Json(RemoteHandleRequest {
                handle: impostor.clone(),
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _) = destroy_sandbox(
            State(n.state.clone()),
            Json(RemoteDestroyRequest {
                handle: impostor,
                purge_volumes: true,
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _) = recover_sandbox(
            State(n.state.clone()),
            Json(RemoteRecoverRequest {
                container_name: "site-web-1".into(),
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(n.provider.seen_ids.lock().unwrap().is_empty());
        assert!(n.provider.destroyed.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn missing_sandbox_is_404_for_exec_and_dead_for_alive() {
        let n = node();
        let gone = sandbox_handle("temps-sandbox-gone", "x");
        let (status, _) = exec_sandbox(State(n.state.clone()), exec_req(gone.clone()))
            .await
            .unwrap_err();
        assert_eq!(status, StatusCode::NOT_FOUND);
        let Json(alive) = sandbox_alive(
            State(n.state.clone()),
            Json(RemoteHandleRequest { handle: gone }),
        )
        .await
        .unwrap();
        assert!(!alive.alive);
        let Json(recovered) = recover_sandbox(
            State(n.state.clone()),
            Json(RemoteRecoverRequest {
                container_name: "gone".into(),
            }),
        )
        .await
        .unwrap();
        assert!(recovered.is_none());
    }

    #[tokio::test]
    async fn destroy_is_idempotent_and_removes_the_work_dir() {
        let n = node();
        let dir = n.state.work_root.join("gone");
        std::fs::create_dir_all(&dir).unwrap();
        let _ = destroy_sandbox(
            State(n.state.clone()),
            Json(RemoteDestroyRequest {
                handle: sandbox_handle("temps-sandbox-gone", "x"),
                purge_volumes: true,
            }),
        )
        .await
        .unwrap();
        assert!(!dir.exists());
        // The container was already gone, so the provider was never asked.
        assert!(n.provider.seen_ids.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn read_file_is_bounded_on_the_worker() {
        let n = node();
        let handle = sandbox_handle("temps-sandbox-abc", "x");
        let Json(contents) = read_sandbox_file(
            State(n.state.clone()),
            Json(RemoteReadFileRequest {
                handle: handle.clone(),
                path: "/small".into(),
            }),
        )
        .await
        .unwrap();
        assert_eq!(contents.contents_b64, "ZGF0YQ==");
        let (status, Json(body)) = read_sandbox_file(
            State(n.state.clone()),
            Json(RemoteReadFileRequest {
                handle,
                path: "/huge".into(),
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body.error.contains("/huge"), "{}", body.error);
        assert_eq!(
            *n.provider.read_limits.lock().unwrap(),
            vec![WORKER_READ_FILE_MAX_BYTES, WORKER_READ_FILE_MAX_BYTES]
        );
    }

    /// Every frame of a finished exec-stream response.
    async fn frames(response: Response) -> (StatusCode, Vec<RemoteExecFrame>) {
        let status = response.status();
        assert_eq!(
            response.headers()[axum::http::header::CONTENT_TYPE],
            "application/x-ndjson"
        );
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap();
        let frames = bytes
            .split(|b| *b == b'\n')
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_slice(l).unwrap())
            .collect();
        (status, frames)
    }

    fn line_of(stream: ExecStream, line: &str) -> (ExecStream, String) {
        (stream, line.to_string())
    }

    #[tokio::test]
    async fn exec_stream_sends_each_line_then_the_exit_code() {
        let n = node_with(
            FakeProvider {
                exec_lines: vec![
                    line_of(ExecStream::Stdout, "ready on :3000"),
                    line_of(ExecStream::Stderr, "deprecation warning"),
                ],
                ..Default::default()
            },
            Vec::new(),
        );
        // A smuggled container id is replaced by the one Docker resolves.
        let handle = sandbox_handle("temps-sandbox-abc", "app-container-id");
        let response = exec_sandbox_stream(State(n.state.clone()), exec_req(handle))
            .await
            .unwrap();
        let (status, frames) = frames(response).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            frames,
            vec![
                RemoteExecFrame::Stdout {
                    line: "ready on :3000".into(),
                    more: false,
                },
                RemoteExecFrame::Stderr {
                    line: "deprecation warning".into(),
                    more: false,
                },
                RemoteExecFrame::Exit { exit_code: 7 },
            ]
        );
        assert_eq!(*n.provider.seen_ids.lock().unwrap(), vec!["real-id"]);
    }

    #[tokio::test]
    async fn root_exec_stream_forwards_stdout_only() {
        let n = node_with(
            FakeProvider {
                exec_lines: vec![
                    line_of(ExecStream::Stdout, "out"),
                    line_of(ExecStream::Stderr, "err"),
                ],
                ..Default::default()
            },
            Vec::new(),
        );
        let mut req = exec_req(sandbox_handle("temps-sandbox-abc", "x"));
        req.0.as_root = true;
        let response = exec_sandbox_stream(State(n.state.clone()), req)
            .await
            .unwrap();
        let (_, frames) = frames(response).await;
        assert_eq!(
            frames,
            vec![
                RemoteExecFrame::Stdout {
                    line: "out".into(),
                    more: false,
                },
                RemoteExecFrame::Exit { exit_code: 0 },
            ]
        );
    }

    #[tokio::test]
    async fn exec_stream_failures_travel_in_band_without_host_paths() {
        let root = tempfile::tempdir().unwrap();
        let reason = format!("exec failed reading {}/abc/out.log", root.path().display());
        let mut n = node_with(
            FakeProvider {
                exec_lines: vec![line_of(ExecStream::Stdout, "before the failure")],
                exec_fails_with: Some(reason),
                ..Default::default()
            },
            Vec::new(),
        );
        // Point the node's work root at the directory named in the error.
        let state = Arc::get_mut(&mut n.state).unwrap();
        state.work_root = root.path().to_path_buf();
        let response = exec_sandbox_stream(
            State(n.state.clone()),
            exec_req(sandbox_handle("temps-sandbox-abc", "x")),
        )
        .await
        .unwrap();
        let (_, frames) = frames(response).await;
        assert_eq!(frames.len(), 2, "{frames:?}");
        match &frames[1] {
            RemoteExecFrame::Error { status, error } => {
                assert_eq!(*status, 500);
                assert!(error.contains(REDACTED_WORK_ROOT), "{error}");
                assert!(!error.contains(&*root.path().to_string_lossy()), "{error}");
            }
            other => panic!("expected an error frame, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn exec_stream_refuses_bad_handles_before_streaming() {
        let n = node();
        let (status, _) = exec_sandbox_stream(
            State(n.state.clone()),
            exec_req(sandbox_handle("temps-sandbox-gone", "x")),
        )
        .await
        .unwrap_err();
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = exec_sandbox_stream(
            State(n.state.clone()),
            exec_req(sandbox_handle("my-app-web-1", "x")),
        )
        .await
        .unwrap_err();
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(n.provider.seen_ids.lock().unwrap().is_empty());

        let no_docker = SandboxHostState::new(None, PathBuf::from("/unused"));
        let (status, _) = exec_sandbox_stream(
            State(Arc::new(no_docker)),
            exec_req(sandbox_handle("temps-sandbox-abc", "x")),
        )
        .await
        .unwrap_err();
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    }

    /// Aborts its task when dropped, like [`StreamedExecGuard`] minus the
    /// process cleanup.
    struct AbortTask(tokio::task::JoinHandle<()>);

    impl Drop for AbortTask {
        fn drop(&mut self) {
            self.0.abort();
        }
    }

    /// Sets its flag when dropped (the task holding it was aborted).
    struct SetOnDrop(Arc<std::sync::atomic::AtomicBool>);

    impl Drop for SetOnDrop {
        fn drop(&mut self) {
            self.0.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn exec_stream_heartbeats_while_quiet_and_stops_the_exec_when_dropped() {
        use futures::StreamExt;
        let (tx, rx) = tokio::sync::mpsc::channel(EXEC_STREAM_CHANNEL_FRAMES);
        let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let guard = SetOnDrop(dropped.clone());
        // A command that never finishes and never prints.
        let task = tokio::spawn(async move {
            let _guard = guard;
            let _tx = tx;
            std::future::pending::<()>().await;
        });
        let response = exec_stream_response(rx, AbortTask(task), Duration::from_millis(20));
        let mut body = response.into_body().into_data_stream();
        for _ in 0..3 {
            let chunk = tokio::time::timeout(Duration::from_secs(5), body.next())
                .await
                .expect("a heartbeat while the command is quiet")
                .unwrap()
                .unwrap();
            let frame: RemoteExecFrame = serde_json::from_slice(chunk.trim_ascii_end()).unwrap();
            assert_eq!(frame, RemoteExecFrame::Heartbeat);
        }
        assert!(!dropped.load(std::sync::atomic::Ordering::SeqCst));
        // The control plane went away.
        drop(body);
        tokio::time::timeout(Duration::from_secs(5), async {
            while !dropped.load(std::sync::atomic::Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the exec task is aborted when the response is dropped");
    }

    /// The exec id the worker tagged the command with.
    fn streamed_exec_id(provider: &FakeProvider) -> String {
        provider.exec_calls.lock().unwrap()[0]
            .1
            .get(STREAMED_EXEC_ID_ENV)
            .cloned()
            .expect("the streamed exec carries its id")
    }

    #[tokio::test]
    async fn a_disconnected_exec_stream_stops_the_commands_processes() {
        let n = node_with(
            FakeProvider {
                exec_hangs: true,
                ..Default::default()
            },
            Vec::new(),
        );
        let response = exec_sandbox_stream(
            State(n.state.clone()),
            exec_req(sandbox_handle("temps-sandbox-abc", "x")),
        )
        .await
        .unwrap();
        // Let the exec start, then the control plane goes away.
        tokio::time::timeout(Duration::from_secs(5), async {
            while n.provider.exec_calls.lock().unwrap().is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the exec starts");
        drop(response);

        let exec_id = streamed_exec_id(&n.provider);
        tokio::time::timeout(Duration::from_secs(5), async {
            while n.provider.exec_calls.lock().unwrap().len() < 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the worker stops the disconnected command");
        let (kill_cmd, _) = n.provider.exec_calls.lock().unwrap()[1].clone();
        assert_eq!(kill_cmd, stop_streamed_exec_cmd(&exec_id));
        // The id travels as an argument, not inside the script.
        assert_eq!(kill_cmd.last(), Some(&exec_id));
        assert!(!kill_cmd[2].contains(&exec_id));
    }

    #[tokio::test]
    async fn a_finished_exec_stream_stops_nothing() {
        let n = node();
        let response = exec_sandbox_stream(
            State(n.state.clone()),
            exec_req(sandbox_handle("temps-sandbox-abc", "x")),
        )
        .await
        .unwrap();
        let (_, frames) = frames(response).await;
        assert_eq!(frames.last(), Some(&RemoteExecFrame::Exit { exit_code: 7 }));
        // Give a wrongly spawned cleanup the chance to run.
        for _ in 0..50 {
            tokio::task::yield_now().await;
        }
        assert_eq!(n.provider.exec_calls.lock().unwrap().len(), 1);
        assert_eq!(streamed_exec_id(&n.provider).len(), 32);
    }

    #[tokio::test]
    async fn long_lines_are_streamed_whole_in_continuation_frames() {
        let long = "z".repeat(temps_agents::sandbox::remote::WORKER_EXEC_LINE_LIMIT + 10);
        let n = node_with(
            FakeProvider {
                exec_lines: vec![line_of(ExecStream::Stdout, &long)],
                ..Default::default()
            },
            Vec::new(),
        );
        let response = exec_sandbox_stream(
            State(n.state.clone()),
            exec_req(sandbox_handle("temps-sandbox-abc", "x")),
        )
        .await
        .unwrap();
        let (_, frames) = frames(response).await;
        assert_eq!(
            frames,
            [
                RemoteExecFrame::output_frames(ExecStream::Stdout, long),
                vec![RemoteExecFrame::Exit { exit_code: 7 }],
            ]
            .concat()
        );
        assert_eq!(frames.len(), 3);
    }

    #[test]
    fn exec_output_keeps_the_tail_within_the_limit() {
        assert_eq!(keep_tail("short".to_string(), 16), "short");
        let kept = keep_tail("0123456789".to_string(), 4);
        assert_eq!(kept, "[6 earlier bytes truncated by the worker node]\n6789");
        // Never splits a multi-byte character.
        let kept = keep_tail("aé€".to_string(), 4);
        assert!(kept.ends_with("€"), "{kept}");
    }

    #[test]
    fn provider_errors_keep_their_meaning_over_http() {
        let status = |e: AgentError| provider_err(e).0;
        assert_eq!(
            status(AgentError::SandboxNotFound {
                run_id: 0,
                sandbox: "test".into()
            }),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            status(AgentError::Validation {
                message: "bad".into()
            }),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            status(AgentError::SandboxProviderUnavailable {
                provider: "docker".into(),
                reason: "daemon down".into(),
            }),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            status(AgentError::SandboxExecFailed {
                run_id: 0,
                sandbox_id: "x".into(),
                reason: "boom".into(),
            }),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(
            status(AgentError::SandboxUnsupportedOnNode {
                sandbox_id: "x".into(),
                node_name: "n".into(),
                feature: "f".into(),
            }),
            StatusCode::UNPROCESSABLE_ENTITY
        );
        // Unrelated "not found" errors must never read as "sandbox gone".
        assert_eq!(
            status(AgentError::RunNotFound { run_id: 3 }),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[tokio::test]
    async fn node_without_docker_answers_503() {
        let root = tempfile::tempdir().unwrap();
        let state = Arc::new(SandboxHostState::new(None, root.path().to_path_buf()));
        let (status, _) = exec_sandbox(
            State(state),
            exec_req(sandbox_handle("temps-sandbox-abc", "real-id")),
        )
        .await
        .unwrap_err();
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn create_and_recover_reject_unsafe_labels() {
        let n = node();
        for label in ["../etc", "egress-proxy-v2-temps-sandbox-abc"] {
            let (status, _) = create_sandbox(State(n.state.clone()), create_req(label))
                .await
                .unwrap_err();
            assert_eq!(status, StatusCode::BAD_REQUEST, "{label}");
        }
        for name in ["my-app-web-1/../x", "a b", "", "egress-proxy-v2-x"] {
            let (status, _) = recover_sandbox(
                State(n.state.clone()),
                Json(RemoteRecoverRequest {
                    container_name: name.into(),
                }),
            )
            .await
            .unwrap_err();
            assert_eq!(status, StatusCode::BAD_REQUEST, "{name:?}");
        }
    }

    #[tokio::test]
    async fn create_never_replaces_a_live_or_foreign_container() {
        let n = node_with(
            FakeProvider::default(),
            vec![("temps-sandbox-foreign", NamedContainer::Other)],
        );
        // `abc` is running; `foreign` is not a sandbox.
        for label in ["abc", "foreign"] {
            let (status, Json(body)) = create_sandbox(State(n.state.clone()), create_req(label))
                .await
                .unwrap_err();
            assert_eq!(status, StatusCode::CONFLICT, "{label}");
            assert!(body.error.contains(label), "{}", body.error);
        }
        assert!(!n.state.work_root.join("abc").exists());
        assert!(n.provider.destroyed.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn create_refuses_a_leftover_work_dir_and_leaves_it_alone() {
        let n = node();
        let dir = n.state.work_root.join("fresh");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("keep.txt"), b"user data").unwrap();
        let (status, Json(body)) = create_sandbox(State(n.state.clone()), create_req("fresh"))
            .await
            .unwrap_err();
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(body.error.contains("fresh"), "{}", body.error);
        assert_eq!(std::fs::read(dir.join("keep.txt")).unwrap(), b"user data");
    }

    #[tokio::test]
    async fn create_replaces_a_stopped_sandbox_keeping_its_work_dir() {
        let n = node_with(
            FakeProvider::default(),
            vec![(
                "temps-sandbox-stale",
                NamedContainer::Sandbox {
                    id: "old".into(),
                    running: false,
                },
            )],
        );
        let dir = n.state.work_root.join("stale");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("keep.txt"), b"user data").unwrap();
        let Json(handle) = create_sandbox(State(n.state.clone()), create_req("stale"))
            .await
            .unwrap();
        assert_eq!(handle.sandbox_name, "temps-sandbox-stale");
        assert_eq!(std::fs::read(dir.join("keep.txt")).unwrap(), b"user data");
    }

    #[tokio::test]
    async fn failed_create_removes_only_the_work_dir_it_created() {
        let failing = || FakeProvider {
            create_fails: true,
            ..Default::default()
        };
        let n = node_with(failing(), Vec::new());
        let (status, _) = create_sandbox(State(n.state.clone()), create_req("new"))
            .await
            .unwrap_err();
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(!n.state.work_root.join("new").exists());

        let n = node_with(
            failing(),
            vec![(
                "temps-sandbox-stale",
                NamedContainer::Sandbox {
                    id: "old".into(),
                    running: false,
                },
            )],
        );
        let dir = n.state.work_root.join("stale");
        std::fs::create_dir_all(&dir).unwrap();
        let _ = create_sandbox(State(n.state.clone()), create_req("stale"))
            .await
            .unwrap_err();
        assert!(dir.exists(), "a pre-existing work dir must survive");
    }

    #[tokio::test]
    async fn concurrent_creates_of_one_label_are_refused() {
        let gate = Arc::new(Notify::new());
        let n = node_with(
            FakeProvider {
                create_gate: Some(gate.clone()),
                ..Default::default()
            },
            Vec::new(),
        );
        let entered = n.provider.create_entered.clone();
        let first = tokio::spawn(create_sandbox(State(n.state.clone()), create_req("dup")));
        entered.notified().await;
        let (status, _) = create_sandbox(State(n.state.clone()), create_req("dup"))
            .await
            .unwrap_err();
        assert_eq!(status, StatusCode::CONFLICT);
        gate.notify_one();
        assert!(first.await.unwrap().is_ok());
        // The claim is released once the create settles.
        assert!(n.state.creating.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn abandoned_create_destroys_the_new_container_and_its_work_dir() {
        let gate = Arc::new(Notify::new());
        let n = node_with(
            FakeProvider {
                create_gate: Some(gate.clone()),
                ..Default::default()
            },
            Vec::new(),
        );
        let entered = n.provider.create_entered.clone();
        // The control plane gives up mid-create: its request (this handler
        // future) is dropped.
        let request = tokio::spawn(create_sandbox(State(n.state.clone()), create_req("orphan")));
        entered.notified().await;
        assert!(n.state.work_root.join("orphan").exists());
        request.abort();
        let _ = request.await;
        // The create still finishes on the worker...
        gate.notify_one();
        // ...and the guard destroys what nobody tracks.
        for _ in 0..200 {
            if !n.provider.destroyed.lock().unwrap().is_empty()
                && !n.state.work_root.join("orphan").exists()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(
            *n.provider.destroyed.lock().unwrap(),
            vec![("temps-sandbox-orphan".to_string(), true)]
        );
        assert!(!n.state.work_root.join("orphan").exists());
        assert!(n.state.creating.lock().unwrap().is_empty());
    }

    // ── write-directory archives ────────────────────────────────────────

    /// One archive entry with a raw header, so tests can build what
    /// `tar::Builder` refuses to (absolute paths, `..`).
    fn raw_entry(
        builder: &mut tar::Builder<Vec<u8>>,
        name: &str,
        kind: tar::EntryType,
        link: Option<&str>,
        data: &[u8],
    ) {
        let mut header = tar::Header::new_gnu();
        header.as_old_mut().name[..name.len()].copy_from_slice(name.as_bytes());
        header.set_entry_type(kind);
        if let Some(link) = link {
            header.as_old_mut().linkname[..link.len()].copy_from_slice(link.as_bytes());
        }
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        if kind == tar::EntryType::GNUSparse {
            if let Some(gnu) = header.as_gnu_mut() {
                gnu.set_real_size(data.len() as u64);
            }
        }
        header.set_cksum();
        builder.append(&header, data).unwrap();
    }

    async fn upload(n: &Node, archive: Vec<u8>) -> Result<(), (StatusCode, String)> {
        write_sandbox_directory(
            State(n.state.clone()),
            Json(RemoteWriteDirectoryRequest {
                handle: sandbox_handle("temps-sandbox-abc", "real-id"),
                target_path: "/home/temps/workspace".into(),
                tar_b64: base64::engine::general_purpose::STANDARD.encode(archive),
            }),
        )
        .await
        .map(|_| ())
        .map_err(|(status, Json(body))| (status, body.error))
    }

    #[tokio::test]
    async fn directory_upload_refuses_links_and_special_files_naming_the_entry() {
        let cases: Vec<(&str, tar::EntryType, Option<&str>)> = vec![
            ("etc-link", tar::EntryType::Symlink, Some("/")),
            (
                "token",
                tar::EntryType::Symlink,
                Some("/var/lib/temps/agent.json"),
            ),
            ("hard", tar::EntryType::Link, Some("ok.txt")),
            ("dev", tar::EntryType::Char, None),
            ("disk", tar::EntryType::Block, None),
            ("pipe", tar::EntryType::Fifo, None),
            ("sparse", tar::EntryType::GNUSparse, None),
        ];
        for (name, kind, link) in cases {
            let n = node();
            let mut builder = tar::Builder::new(Vec::new());
            raw_entry(&mut builder, "ok.txt", tar::EntryType::Regular, None, b"ok");
            raw_entry(&mut builder, name, kind, link, b"");
            let (status, message) = upload(&n, builder.into_inner().unwrap()).await.unwrap_err();
            assert_eq!(status, StatusCode::BAD_REQUEST, "{name}: {message}");
            assert!(message.contains(name), "{name}: {message}");
            assert!(message.contains("temps-sandbox-abc"), "{message}");
            assert!(
                n.provider.seen_ids.lock().unwrap().is_empty(),
                "{name}: the provider must not see a refused archive"
            );
        }
    }

    #[tokio::test]
    async fn directory_upload_refuses_absolute_and_parent_paths() {
        for name in ["/etc/cron.d/x", "../escaped.txt", "a/../../escaped.txt"] {
            let n = node();
            let mut builder = tar::Builder::new(Vec::new());
            raw_entry(&mut builder, name, tar::EntryType::Regular, None, b"hi");
            let (status, message) = upload(&n, builder.into_inner().unwrap()).await.unwrap_err();
            assert_eq!(status, StatusCode::BAD_REQUEST, "{name}: {message}");
            assert!(message.contains(name), "{name}: {message}");
            assert!(!n.state.work_root.join("escaped.txt").exists());
            assert!(!n
                .state
                .work_root
                .parent()
                .unwrap()
                .join("escaped.txt")
                .exists());
            assert!(n.provider.seen_ids.lock().unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn directory_upload_refuses_archives_that_unpack_past_the_limit() {
        let n = node();
        // A header claiming more data than the limit; nothing is written.
        let mut header = tar::Header::new_gnu();
        header.set_path("big.bin").unwrap();
        header.set_size(MAX_UPLOAD_UNPACKED_BYTES + 1);
        header.set_mode(0o644);
        header.set_cksum();
        let mut archive = header.as_bytes().to_vec();
        archive.extend_from_slice(&[0u8; 1024]);
        let (status, message) = upload(&n, archive).await.unwrap_err();
        assert_eq!(status, StatusCode::BAD_REQUEST, "{message}");
        assert!(message.contains("big.bin"), "{message}");
    }

    #[tokio::test]
    async fn directory_upload_round_trips_the_control_plane_archive() {
        let source = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(source.path().join("skills/deep/er")).unwrap();
        std::fs::write(source.path().join("CLAUDE.md"), b"# notes\n").unwrap();
        std::fs::write(
            source.path().join("skills/deep/er/run.sh"),
            b"#!/bin/sh\necho hi\n",
        )
        .unwrap();
        std::fs::write(
            source.path().join("skills/bin.dat"),
            [0u8, 159, 146, 150, 255],
        )
        .unwrap();
        #[cfg(unix)]
        {
            // In-tree link: arrives as a plain copy. Escaping link: dropped.
            std::os::unix::fs::symlink("CLAUDE.md", source.path().join("AGENTS.md")).unwrap();
            std::os::unix::fs::symlink("/", source.path().join("root")).unwrap();
        }

        let archive = temps_agents::sandbox::remote::tar_directory(source.path()).unwrap();
        let n = node();
        upload(&n, archive).await.unwrap();

        let mut expected = vec![
            ("CLAUDE.md".to_string(), b"# notes\n".to_vec()),
            ("skills/bin.dat".to_string(), vec![0u8, 159, 146, 150, 255]),
            (
                "skills/deep/er/run.sh".to_string(),
                b"#!/bin/sh\necho hi\n".to_vec(),
            ),
        ];
        #[cfg(unix)]
        expected.push(("AGENTS.md".to_string(), b"# notes\n".to_vec()));
        expected.sort();
        let mut got = n.provider.uploaded.lock().unwrap().clone();
        got.sort();
        assert_eq!(got, expected);
        // Staging is gone afterwards.
        let leftovers: Vec<_> = std::fs::read_dir(&n.state.work_root)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    // ── upload concurrency ──────────────────────────────────────────────

    #[tokio::test]
    async fn uploads_beyond_the_limit_wait_then_get_503() {
        use axum::body::Body;
        use tower::ServiceExt;

        let root = tempfile::tempdir().unwrap();
        let state = Arc::new(
            SandboxHostState::new(None, root.path().to_path_buf())
                .with_upload_permit_wait(Duration::from_millis(200)),
        );
        let release = Arc::new(tokio::sync::Semaphore::new(0));
        let started = Arc::new(tokio::sync::Semaphore::new(0));
        let (release_h, started_h) = (release.clone(), started.clone());
        let app = axum::Router::new().route(
            "/upload",
            axum::routing::post(move || {
                let (release, started) = (release_h.clone(), started_h.clone());
                async move {
                    started.add_permits(1);
                    if let Ok(permit) = release.acquire().await {
                        permit.forget();
                    }
                    StatusCode::OK
                }
            })
            .layer(axum::middleware::from_fn_with_state(
                state.clone(),
                limit_uploads,
            )),
        );
        let request = || {
            axum::http::Request::post("/upload")
                .body(Body::empty())
                .unwrap()
        };
        let busy: Vec<_> = (0..MAX_CONCURRENT_UPLOADS)
            .map(|_| tokio::spawn(app.clone().oneshot(request())))
            .collect();
        let _ = started
            .acquire_many(MAX_CONCURRENT_UPLOADS as u32)
            .await
            .unwrap();
        let refused = app.clone().oneshot(request()).await.unwrap();
        assert_eq!(refused.status(), StatusCode::SERVICE_UNAVAILABLE);
        release.add_permits(MAX_CONCURRENT_UPLOADS);
        for task in busy {
            assert_eq!(task.await.unwrap().unwrap().status(), StatusCode::OK);
        }
        // Slots free again once the uploads finish.
        let again = tokio::spawn(app.clone().oneshot(request()));
        let _ = started.acquire().await.unwrap();
        release.add_permits(1);
        assert_eq!(again.await.unwrap().unwrap().status(), StatusCode::OK);
    }
}
