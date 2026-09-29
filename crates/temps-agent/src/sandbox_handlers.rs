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
//! daemon by sandbox *name*: the container id in the request is ignored, so a
//! handle cannot point a sandbox operation at an application or service
//! container.

use axum::{extract::State, http::StatusCode, Json};
use base64::Engine;
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use temps_agents::sandbox::remote::{
    is_sandbox_handle, is_valid_sandbox_label, RemoteAliveResponse, RemoteCreateRequest,
    RemoteDestroyRequest, RemoteErrorBody, RemoteExecRequest, RemoteExecResponse,
    RemoteFileContents, RemoteHandleRequest, RemoteKillRequest, RemoteReadFileRequest,
    RemoteRecoverRequest, RemoteStatusResponse, RemoteWriteDirectoryRequest,
    RemoteWriteFileRequest, SANDBOX_CONTAINER_PREFIX,
};
use temps_agents::sandbox::{SandboxCreateConfig, SandboxHandle, SandboxProvider};

/// Finds containers by exact name on this node's Docker daemon.
#[async_trait::async_trait]
pub trait ContainerLookup: Send + Sync {
    /// Id of the container named exactly `name`; `None` if there is none.
    async fn container_id(&self, name: &str) -> Result<Option<String>, String>;
}

#[async_trait::async_trait]
impl ContainerLookup for bollard::Docker {
    async fn container_id(&self, name: &str) -> Result<Option<String>, String> {
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
                Ok(info.id)
            }
            Ok(_) => Ok(None),
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => Ok(None),
            Err(e) => Err(e.to_string()),
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
}

impl SandboxHostState {
    pub fn new(host: Option<SandboxHost>, work_root: PathBuf) -> Self {
        Self { host, work_root }
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
/// by that name. `Ok(None)` = no such sandbox container on this node.
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
    match host.containers.container_id(&handle.sandbox_name).await {
        Ok(Some(id)) => {
            handle.sandbox_id = id;
            Ok(Some(handle))
        }
        Ok(None) => Ok(None),
        Err(e) => Err(err(
            StatusCode::SERVICE_UNAVAILABLE,
            format!("the Docker daemon on this node failed: {}", e),
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
/// 404 = the container is gone, 400 = bad request, 503 = this node can't
/// serve sandboxes right now, 500 = the operation itself failed.
fn provider_err(e: temps_agents::error::AgentError) -> ApiError {
    use temps_agents::error::AgentError;
    let status = match &e {
        AgentError::SandboxNotFound { .. } => StatusCode::NOT_FOUND,
        AgentError::Validation { .. } => StatusCode::BAD_REQUEST,
        AgentError::SandboxProviderUnavailable { .. }
        | AgentError::SandboxNodeUnavailable { .. } => StatusCode::SERVICE_UNAVAILABLE,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    err(status, e.to_string())
}

#[derive(Serialize)]
pub struct OkBody {
    ok: bool,
}

fn ok() -> Json<OkBody> {
    Json(OkBody { ok: true })
}

fn work_dir_for(state: &SandboxHostState, label: &str) -> PathBuf {
    state.work_root.join(label)
}

fn label_of(handle: &SandboxHandle) -> Option<&str> {
    handle.sandbox_name.strip_prefix(SANDBOX_CONTAINER_PREFIX)
}

/// `POST /agent/sandboxes`
pub async fn create_sandbox(
    State(state): HostState,
    Json(req): Json<RemoteCreateRequest>,
) -> ApiResult<SandboxHandle> {
    let provider = &host(&state)?.provider;
    if !is_valid_sandbox_label(&req.label) {
        return Err(err(
            StatusCode::BAD_REQUEST,
            format!("invalid sandbox label '{}'", req.label),
        ));
    }
    let host_work_dir = work_dir_for(&state, &req.label);
    tokio::fs::create_dir_all(&host_work_dir)
        .await
        .map_err(|e| {
            err(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("create work dir {}: {}", host_work_dir.display(), e),
            )
        })?;
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
    match provider.create(config).await {
        Ok(handle) => {
            tracing::info!(sandbox = %handle.sandbox_name, "Created sandbox for control plane");
            Ok(Json(handle))
        }
        Err(e) => {
            let _ = tokio::fs::remove_dir_all(&host_work_dir).await;
            Err(provider_err(e))
        }
    }
}

/// `POST /agent/sandboxes/exec`
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
        stdout: keep_tail(result.stdout, EXEC_OUTPUT_LIMIT),
        stderr: keep_tail(result.stderr, EXEC_OUTPUT_LIMIT),
    }))
}

/// Most exec output (per stream) a worker returns to the control plane,
/// which buffers the whole response: without a cap, one noisy command on a
/// worker could exhaust the control plane's memory.
const EXEC_OUTPUT_LIMIT: usize = 16 * 1024 * 1024;

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

/// `POST /agent/sandboxes/read-file`
pub async fn read_sandbox_file(
    State(state): HostState,
    Json(req): Json<RemoteReadFileRequest>,
) -> ApiResult<RemoteFileContents> {
    let host = host(&state)?;
    let handle = require_handle(host, req.handle).await?;
    let bytes = host
        .provider
        .read_file(&handle, &req.path)
        .await
        .map_err(provider_err)?;
    Ok(Json(RemoteFileContents {
        contents_b64: base64::engine::general_purpose::STANDARD.encode(bytes),
    }))
}

/// `POST /agent/sandboxes/write-file`
pub async fn write_sandbox_file(
    State(state): HostState,
    Json(req): Json<RemoteWriteFileRequest>,
) -> ApiResult<OkBody> {
    let host = host(&state)?;
    let handle = require_handle(host, req.handle).await?;
    let contents = base64::engine::general_purpose::STANDARD
        .decode(req.contents_b64)
        .map_err(|e| err(StatusCode::BAD_REQUEST, format!("invalid base64: {}", e)))?;
    host.provider
        .write_file(&handle, &req.path, &contents, req.mode)
        .await
        .map_err(provider_err)?;
    Ok(ok())
}

/// `POST /agent/sandboxes/write-directory`
pub async fn write_sandbox_directory(
    State(state): HostState,
    Json(req): Json<RemoteWriteDirectoryRequest>,
) -> ApiResult<OkBody> {
    let host = host(&state)?;
    let handle = require_handle(host, req.handle).await?;
    let archive = base64::engine::general_purpose::STANDARD
        .decode(req.tar_b64)
        .map_err(|e| err(StatusCode::BAD_REQUEST, format!("invalid base64: {}", e)))?;
    tokio::fs::create_dir_all(&state.work_root)
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let staging = tempfile::Builder::new()
        .prefix(".incoming-")
        .tempdir_in(&state.work_root)
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let dest = staging.path().to_path_buf();
    // `staging` is a fresh empty directory, and `Archive::unpack` refuses
    // entries that would escape `dest` (`..`, absolute paths, writes through
    // symlinks), so a hostile archive cannot write outside staging. Owner,
    // mode bits and xattrs from the archive are never applied to the host.
    tokio::task::spawn_blocking(move || {
        let mut tar = tar::Archive::new(archive.as_slice());
        tar.set_preserve_permissions(false);
        tar.set_preserve_ownerships(false);
        tar.set_unpack_xattrs(false);
        tar.unpack(&dest)
    })
    .await
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map_err(|e| err(StatusCode::BAD_REQUEST, format!("invalid archive: {}", e)))?;
    host.provider
        .write_directory(&handle, staging.path(), &req.target_path)
        .await
        .map_err(provider_err)?;
    Ok(ok())
}

/// `POST /agent/sandboxes/kill-processes`
pub async fn kill_sandbox_processes(
    State(state): HostState,
    Json(req): Json<RemoteKillRequest>,
) -> ApiResult<OkBody> {
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
pub async fn destroy_sandbox(
    State(state): HostState,
    Json(req): Json<RemoteDestroyRequest>,
) -> ApiResult<OkBody> {
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
        let dir = work_dir_for(&state, label);
        if let Err(e) = tokio::fs::remove_dir_all(&dir).await {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(dir = %dir.display(), "Failed to remove sandbox work dir: {}", e);
            }
        }
    }
    Ok(ok())
}

/// `POST /agent/sandboxes/stop`
pub async fn stop_sandbox(
    State(state): HostState,
    Json(req): Json<RemoteHandleRequest>,
) -> ApiResult<OkBody> {
    let host = host(&state)?;
    let handle = require_handle(host, req.handle).await?;
    host.provider.stop(&handle).await.map_err(provider_err)?;
    Ok(ok())
}

/// `POST /agent/sandboxes/start`
pub async fn start_sandbox(
    State(state): HostState,
    Json(req): Json<RemoteHandleRequest>,
) -> ApiResult<OkBody> {
    let host = host(&state)?;
    let handle = require_handle(host, req.handle).await?;
    host.provider.start(&handle).await.map_err(provider_err)?;
    Ok(ok())
}

/// `POST /agent/sandboxes/recover` — find a sandbox container by name after
/// a control-plane restart.
pub async fn recover_sandbox(
    State(state): HostState,
    Json(req): Json<RemoteRecoverRequest>,
) -> ApiResult<Option<SandboxHandle>> {
    let provider = &host(&state)?.provider;
    // The wire carries the bare label; the provider adds the
    // `temps-sandbox-` prefix itself, so recovery can never reach the node's
    // deployment or service containers.
    if !is_valid_sandbox_label(&req.container_name) {
        return Err(err(
            StatusCode::BAD_REQUEST,
            format!("invalid sandbox name '{}'", req.container_name),
        ));
    }
    let handle = provider
        .recover_by_name(&req.container_name)
        .await
        .map_err(provider_err)?;
    Ok(Json(handle))
}

/// `POST /agent/sandboxes/status` — whether this node can run sandboxes.
pub async fn sandbox_status(State(state): HostState) -> ApiResult<RemoteStatusResponse> {
    let Some(SandboxHost { provider, .. }) = state.host.as_ref() else {
        return Ok(Json(RemoteStatusResponse {
            available: false,
            image_ready: false,
            image: String::new(),
        }));
    };
    let available = provider.is_available().await;
    let (image_ready, image) = provider
        .image_status()
        .await
        .unwrap_or((false, String::new()));
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
    use temps_agents::ai_cli::OnEventCallback;
    use temps_agents::error::AgentError;
    use temps_agents::sandbox::{KillSignal, SandboxBackend, SandboxExecResult};

    /// Records the container id every call was made against.
    #[derive(Default)]
    struct FakeProvider {
        seen_ids: Mutex<Vec<String>>,
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
            _cmd: Vec<String>,
            _env: HashMap<String, String>,
            _on_output: Option<OnEventCallback>,
        ) -> Result<SandboxExecResult, AgentError> {
            self.saw(handle);
            Ok(SandboxExecResult {
                exit_code: 0,
                stdout: "ok".into(),
                stderr: String::new(),
            })
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
        async fn write_directory(
            &self,
            handle: &SandboxHandle,
            _local_dir: &std::path::Path,
            _target_path: &str,
        ) -> Result<(), AgentError> {
            self.saw(handle);
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
        async fn destroy(&self, handle: &SandboxHandle, _purge: bool) -> Result<(), AgentError> {
            self.saw(handle);
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

    /// Containers on the fake node, by exact name.
    struct FakeContainers(HashMap<String, String>);

    #[async_trait::async_trait]
    impl ContainerLookup for FakeContainers {
        async fn container_id(&self, name: &str) -> Result<Option<String>, String> {
            Ok(self.0.get(name).cloned())
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

    /// A node hosting one sandbox, `temps-sandbox-abc`, whose real
    /// container id is `real-id`.
    fn node() -> Node {
        let root = tempfile::tempdir().unwrap();
        let provider = Arc::new(FakeProvider::default());
        let containers = FakeContainers(HashMap::from([(
            "temps-sandbox-abc".to_string(),
            "real-id".to_string(),
        )]));
        let state = Arc::new(SandboxHostState::new(
            Some(SandboxHost {
                provider: provider.clone(),
                containers: Arc::new(containers),
            }),
            root.path().to_path_buf(),
        ));
        Node {
            state,
            provider,
            _root: root,
        }
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
        let handle = sandbox_handle("my-app-web-1", "real-id");
        let (status, _) = exec_sandbox(State(n.state.clone()), exec_req(handle))
            .await
            .unwrap_err();
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(n.provider.seen_ids.lock().unwrap().is_empty());
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
        use temps_agents::error::AgentError;
        let status = |e: AgentError| provider_err(e).0;
        assert_eq!(
            status(AgentError::SandboxNotFound { run_id: 0 }),
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
        let (status, _) = create_sandbox(
            State(n.state.clone()),
            Json(RemoteCreateRequest {
                run_id: 1,
                label: "../etc".into(),
                image: None,
                cpu_limit: None,
                memory_limit_mb: None,
                pids_limit: None,
                disk_size_mb: None,
                network_mode: None,
                env_vars: HashMap::new(),
                idle_timeout_secs: 60,
                backend: None,
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(status, StatusCode::BAD_REQUEST);
        for name in ["my-app-web-1/../x", "a b", ""] {
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
    async fn directory_upload_rejects_entries_escaping_staging() {
        let n = node();
        // Hand-built header: tar::Builder refuses to write `..` paths.
        let mut header = tar::Header::new_gnu();
        let evil = b"../escaped.txt";
        header.as_old_mut().name[..evil.len()].copy_from_slice(evil);
        header.set_size(2);
        header.set_mode(0o644);
        header.set_cksum();
        let mut builder = tar::Builder::new(Vec::new());
        builder.append(&header, &b"hi"[..]).unwrap();
        let archive = builder.into_inner().unwrap();

        let result = write_sandbox_directory(
            State(n.state.clone()),
            Json(RemoteWriteDirectoryRequest {
                handle: sandbox_handle("temps-sandbox-abc", "real-id"),
                target_path: "/home/temps/workspace".into(),
                tar_b64: base64::engine::general_purpose::STANDARD.encode(archive),
            }),
        )
        .await;
        assert!(!n.state.work_root.join("escaped.txt").exists());
        assert!(!n
            .state
            .work_root
            .parent()
            .unwrap()
            .join("escaped.txt")
            .exists());
        // `unpack` skips `..` entries rather than failing; either way nothing
        // lands outside staging and the provider only sees staging.
        if let Err((status, _)) = result {
            assert_eq!(status, StatusCode::BAD_REQUEST);
        }
    }
}
