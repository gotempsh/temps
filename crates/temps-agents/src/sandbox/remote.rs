// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Remote sandbox provider (ADR-048).
//!
//! The control plane drives a sandbox that lives on a worker node through
//! the worker agent's `/agent/sandboxes/*` API. The worker runs the same
//! `DockerSandboxProvider` the control plane uses locally, so this type is
//! a thin HTTP client: every call serialises its arguments, the worker
//! replays them against its local provider, and the result comes back.
//!
//! The wire types live here (not in the worker crate) so both ends compile
//! against one definition.
//!
//! Phase-1 limits, each surfaced as an explicit error instead of a silent
//! fallback: interactive terminals, the retained agent runtime, snapshots,
//! disk resize and application networks are not available for sandboxes on
//! worker nodes yet. `exec` output is returned when the command finishes;
//! callbacks receive the lines afterwards rather than live.

use async_trait::async_trait;
use base64::Engine;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;

use super::{
    ExecStream, KillSignal, OnStreamEventCallback, PtyAttachment, SandboxBackend,
    SandboxCreateConfig, SandboxExecResult, SandboxHandle, SandboxProvider, SnapshotArtifact,
};
use crate::ai_cli::OnEventCallback;
use crate::error::AgentError;

/// Name reported by [`RemoteSandboxProvider::name`].
pub const REMOTE_PROVIDER_NAME: &str = "remote";

/// Timeout for lifecycle calls (create pulls the image on first use).
const LIFECYCLE_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// Timeout for `exec`. Matches the longest command the sandbox API accepts.
const EXEC_TIMEOUT: Duration = Duration::from_secs(60 * 60);
/// Timeout for cheap calls (liveness, file IO, recovery lookups).
const SHORT_TIMEOUT: Duration = Duration::from_secs(120);

// ── Wire types ──────────────────────────────────────────────────────────

/// `POST /agent/sandboxes` — create a sandbox on the worker.
///
/// There is deliberately no host path in this request: the worker derives
/// the sandbox work directory from `label` under its own data directory, so
/// the control plane can never ask a worker to bind-mount an arbitrary path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteCreateRequest {
    pub run_id: i32,
    /// Container suffix (`temps-sandbox-<label>`). Validated by the worker
    /// with [`is_valid_sandbox_label`].
    pub label: String,
    pub image: Option<String>,
    pub cpu_limit: Option<f64>,
    pub memory_limit_mb: Option<u64>,
    pub pids_limit: Option<i64>,
    pub disk_size_mb: Option<u64>,
    pub network_mode: Option<String>,
    pub env_vars: HashMap<String, String>,
    pub idle_timeout_secs: u64,
    pub backend: Option<SandboxBackend>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteHandleRequest {
    pub handle: SandboxHandle,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteDestroyRequest {
    pub handle: SandboxHandle,
    pub purge_volumes: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteExecRequest {
    pub handle: SandboxHandle,
    pub cmd: Vec<String>,
    pub env: HashMap<String, String>,
    /// Run as this user (`exec_as_user`). Ignored when `as_root` is set.
    pub user: Option<String>,
    pub as_root: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteExecResponse {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteReadFileRequest {
    pub handle: SandboxHandle,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteFileContents {
    /// Base64 (standard alphabet) file bytes.
    pub contents_b64: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteWriteFileRequest {
    pub handle: SandboxHandle,
    pub path: String,
    pub contents_b64: String,
    pub mode: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteWriteDirectoryRequest {
    pub handle: SandboxHandle,
    pub target_path: String,
    /// Base64 tar archive of the directory contents (paths relative to the
    /// directory root).
    pub tar_b64: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteKillRequest {
    pub handle: SandboxHandle,
    pub pattern: String,
    pub signal: KillSignal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteRecoverRequest {
    /// Sandbox label, without the `temps-sandbox-` prefix.
    pub container_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteAliveResponse {
    pub alive: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteStatusResponse {
    pub available: bool,
    pub image_ready: bool,
    pub image: String,
}

/// Error body returned by every `/agent/sandboxes/*` endpoint on failure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteErrorBody {
    pub error: String,
}

/// Container-name prefix every sandbox container carries. The worker only
/// operates on handles whose name has this prefix, so the sandbox API can
/// never be pointed at an application or service container.
pub const SANDBOX_CONTAINER_PREFIX: &str = "temps-sandbox-";

/// A sandbox label is the container suffix and the name of the worker-side
/// work directory, so it must be a single safe path segment.
pub fn is_valid_sandbox_label(label: &str) -> bool {
    !label.is_empty()
        && label.len() <= 64
        && label
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// True when `handle` names a sandbox container this API may operate on.
pub fn is_sandbox_handle(handle: &SandboxHandle) -> bool {
    handle
        .sandbox_name
        .strip_prefix(SANDBOX_CONTAINER_PREFIX)
        .is_some_and(is_valid_sandbox_label)
}

// ── Client ──────────────────────────────────────────────────────────────

/// Drives sandboxes on one worker node through its agent API.
pub struct RemoteSandboxProvider {
    node_id: i32,
    node_name: String,
    agent_url: String,
    token: String,
    client: reqwest::Client,
    defaults: RemoteSandboxDefaults,
}

/// Control-plane sandbox defaults applied when a create request leaves them
/// unset, so a sandbox gets the operator's configured image and limits
/// wherever it runs — the worker's own provider only knows built-in defaults.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RemoteSandboxDefaults {
    pub image: Option<String>,
    pub cpu_limit: Option<f64>,
    pub memory_limit_mb: Option<u64>,
    /// The operator's sandbox network mode. The worker's own provider is
    /// built with defaults, so without this a worker sandbox would get the
    /// default (`full`) even where the operator chose `none`.
    pub network_mode: Option<String>,
}

impl RemoteSandboxProvider {
    /// `client` must already carry the node transport configuration (mTLS
    /// identity + cluster CA for `https://` agents), as built by
    /// `temps_deployments::cluster_ca::build_node_http_client`.
    pub fn new(
        node_id: i32,
        node_name: String,
        agent_url: String,
        token: String,
        client: reqwest::Client,
    ) -> Self {
        Self {
            node_id,
            node_name,
            agent_url: agent_url.trim_end_matches('/').to_string(),
            token,
            client,
            defaults: RemoteSandboxDefaults::default(),
        }
    }

    pub fn with_defaults(mut self, defaults: RemoteSandboxDefaults) -> Self {
        self.defaults = defaults;
        self
    }

    pub fn node_id(&self) -> i32 {
        self.node_id
    }

    pub fn node_name(&self) -> &str {
        &self.node_name
    }

    fn unavailable(&self, reason: impl std::fmt::Display) -> AgentError {
        AgentError::SandboxProviderUnavailable {
            provider: format!("node '{}'", self.node_name),
            reason: format!(
                "worker node '{}' (id {}) is unavailable: {}",
                self.node_name, self.node_id, reason
            ),
        }
    }

    fn failed(
        &self,
        handle: Option<&SandboxHandle>,
        operation: &str,
        reason: String,
    ) -> AgentError {
        AgentError::SandboxExecFailed {
            run_id: 0,
            sandbox_id: handle.map(|h| h.sandbox_id.clone()).unwrap_or_default(),
            reason: format!(
                "{} on worker node '{}' failed: {}",
                operation, self.node_name, reason
            ),
        }
    }

    fn unsupported(&self, handle: &SandboxHandle, feature: &str) -> AgentError {
        AgentError::SandboxExecFailed {
            run_id: 0,
            sandbox_id: handle.sandbox_id.clone(),
            reason: format!(
                "{} is not available yet for sandboxes on worker nodes (sandbox is on node '{}'). \
                 Create the sandbox on the control plane to use it.",
                feature, self.node_name
            ),
        }
    }

    /// POST `body` to `path`, returning the decoded JSON response. Transport
    /// failures and 502/503/504 (the node, not the request, is the problem —
    /// e.g. its Docker daemon is down) become `SandboxProviderUnavailable`;
    /// any other non-2xx answer carries the worker's error message.
    async fn call<B: Serialize + ?Sized, R: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
        timeout: Duration,
        handle: Option<&SandboxHandle>,
        operation: &str,
    ) -> Result<R, AgentError> {
        let url = format!("{}{}", self.agent_url, path);
        let response = self
            .client
            .post(&url)
            .bearer_auth(&self.token)
            .timeout(timeout)
            .json(body)
            .send()
            .await
            .map_err(|e| self.unavailable(e))?;
        let status = response.status();
        if !status.is_success() {
            let message = response
                .json::<RemoteErrorBody>()
                .await
                .ok()
                .map(|b| b.error);
            return Err(match (status, message) {
                (
                    reqwest::StatusCode::BAD_GATEWAY
                    | reqwest::StatusCode::SERVICE_UNAVAILABLE
                    | reqwest::StatusCode::GATEWAY_TIMEOUT,
                    message,
                ) => self.unavailable(message.unwrap_or_else(|| format!("HTTP {}", status))),
                // The node refused our credentials: nothing on this side can
                // fix that, the node has to be enrolled again.
                (reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN, _) => self
                    .unavailable(
                        "the node rejected the control plane's credentials; \
                         re-join it with `temps join`",
                    ),
                // The worker reports a missing container with an error body;
                // callers treat that as "already gone", as they do locally.
                (reqwest::StatusCode::NOT_FOUND, Some(_)) => {
                    AgentError::SandboxNotFound { run_id: 0 }
                }
                (reqwest::StatusCode::BAD_REQUEST, Some(message)) => {
                    AgentError::Validation { message }
                }
                (_, Some(message)) => self.failed(handle, operation, message),
                // A worker agent older than ADR-048 has no sandbox routes, so
                // axum answers a bare 404 with no error body.
                (reqwest::StatusCode::NOT_FOUND, None) => self.unavailable(
                    "the agent on this node does not support sandboxes; upgrade temps on the node",
                ),
                (_, None) => self.failed(handle, operation, format!("HTTP {}", status)),
            });
        }
        response
            .json::<R>()
            .await
            .map_err(|e| self.failed(handle, operation, format!("invalid response: {}", e)))
    }

    fn stamp(&self, mut handle: SandboxHandle) -> SandboxHandle {
        handle.node_id = Some(self.node_id);
        handle
    }

    /// Strip the routing stamp before sending a handle back to the worker —
    /// on the worker the sandbox is local.
    fn local_handle(handle: &SandboxHandle) -> SandboxHandle {
        let mut h = handle.clone();
        h.node_id = None;
        h
    }

    async fn run_exec(
        &self,
        handle: &SandboxHandle,
        cmd: Vec<String>,
        env: HashMap<String, String>,
        user: Option<String>,
        as_root: bool,
    ) -> Result<SandboxExecResult, AgentError> {
        let body = RemoteExecRequest {
            handle: Self::local_handle(handle),
            cmd,
            env,
            user,
            as_root,
        };
        let r: RemoteExecResponse = self
            .call(
                "/agent/sandboxes/exec",
                &body,
                EXEC_TIMEOUT,
                Some(handle),
                "exec",
            )
            .await?;
        Ok(SandboxExecResult {
            exit_code: r.exit_code,
            stdout: r.stdout,
            stderr: r.stderr,
        })
    }

    async fn replay_lines(result: &SandboxExecResult, on_output: Option<OnEventCallback>) {
        if let Some(cb) = on_output {
            for line in result.stdout.lines() {
                cb(line.to_string()).await;
            }
        }
    }
}

/// Build a tar archive of `dir` in memory (paths relative to `dir`).
fn tar_directory(dir: &std::path::Path) -> Result<Vec<u8>, std::io::Error> {
    let mut builder = tar::Builder::new(Vec::new());
    builder.follow_symlinks(false);
    builder.append_dir_all(".", dir)?;
    builder.into_inner()
}

#[async_trait]
impl SandboxProvider for RemoteSandboxProvider {
    async fn create(&self, config: SandboxCreateConfig) -> Result<SandboxHandle, AgentError> {
        let label = config
            .container_name_override
            .clone()
            .unwrap_or_else(|| config.run_id.to_string());
        if !is_valid_sandbox_label(&label) {
            return Err(AgentError::Validation {
                message: format!("invalid sandbox label '{}'", label),
            });
        }
        if config.workspace_volume.is_some() {
            return Err(AgentError::SandboxCreationFailed {
                run_id: config.run_id,
                provider: REMOTE_PROVIDER_NAME.into(),
                reason: format!(
                    "workspace volumes are not available yet for sandboxes on worker nodes \
                     (requested node '{}')",
                    self.node_name
                ),
            });
        }
        if config.backend == Some(SandboxBackend::Firecracker) {
            return Err(AgentError::SandboxCreationFailed {
                run_id: config.run_id,
                provider: REMOTE_PROVIDER_NAME.into(),
                reason: format!(
                    "the firecracker backend is not available yet on worker nodes \
                     (requested node '{}'); use the docker backend",
                    self.node_name
                ),
            });
        }
        let body = RemoteCreateRequest {
            run_id: config.run_id,
            label,
            image: config.image.or_else(|| self.defaults.image.clone()),
            cpu_limit: config.cpu_limit.or(self.defaults.cpu_limit),
            memory_limit_mb: config.memory_limit_mb.or(self.defaults.memory_limit_mb),
            pids_limit: config.pids_limit,
            disk_size_mb: config.disk_size_mb,
            network_mode: config
                .network_mode
                .or_else(|| self.defaults.network_mode.clone()),
            env_vars: config.env_vars,
            idle_timeout_secs: config.idle_timeout.as_secs(),
            backend: config.backend,
        };
        let handle: SandboxHandle = self
            .call("/agent/sandboxes", &body, LIFECYCLE_TIMEOUT, None, "create")
            .await
            .map_err(|e| match e {
                AgentError::SandboxExecFailed { reason, .. } => AgentError::SandboxCreationFailed {
                    run_id: config.run_id,
                    provider: REMOTE_PROVIDER_NAME.into(),
                    reason,
                },
                other => other,
            })?;
        Ok(self.stamp(handle))
    }

    async fn exec(
        &self,
        handle: &SandboxHandle,
        cmd: Vec<String>,
        env: HashMap<String, String>,
        on_output: Option<OnEventCallback>,
    ) -> Result<SandboxExecResult, AgentError> {
        let result = self.run_exec(handle, cmd, env, None, false).await?;
        Self::replay_lines(&result, on_output).await;
        Ok(result)
    }

    async fn exec_as_root(
        &self,
        handle: &SandboxHandle,
        cmd: Vec<String>,
        env: HashMap<String, String>,
        on_output: Option<OnEventCallback>,
    ) -> Result<SandboxExecResult, AgentError> {
        let result = self.run_exec(handle, cmd, env, None, true).await?;
        Self::replay_lines(&result, on_output).await;
        Ok(result)
    }

    async fn exec_as_user(
        &self,
        handle: &SandboxHandle,
        user: &str,
        cmd: Vec<String>,
        env: HashMap<String, String>,
        on_output: Option<OnEventCallback>,
    ) -> Result<SandboxExecResult, AgentError> {
        let result = self
            .run_exec(handle, cmd, env, Some(user.to_string()), false)
            .await?;
        Self::replay_lines(&result, on_output).await;
        Ok(result)
    }

    async fn exec_streamed(
        &self,
        handle: &SandboxHandle,
        cmd: Vec<String>,
        env: HashMap<String, String>,
        on_event: Option<OnStreamEventCallback>,
    ) -> Result<SandboxExecResult, AgentError> {
        let result = self.run_exec(handle, cmd, env, None, false).await?;
        if let Some(cb) = on_event {
            for line in result.stdout.lines() {
                cb(ExecStream::Stdout, line.to_string()).await;
            }
            for line in result.stderr.lines() {
                cb(ExecStream::Stderr, line.to_string()).await;
            }
        }
        Ok(result)
    }

    async fn is_alive(&self, handle: &SandboxHandle) -> Result<bool, AgentError> {
        let body = RemoteHandleRequest {
            handle: Self::local_handle(handle),
        };
        let r: RemoteAliveResponse = self
            .call(
                "/agent/sandboxes/alive",
                &body,
                SHORT_TIMEOUT,
                Some(handle),
                "is_alive",
            )
            .await?;
        Ok(r.alive)
    }

    async fn write_file(
        &self,
        handle: &SandboxHandle,
        path: &str,
        contents: &[u8],
        mode: u32,
    ) -> Result<(), AgentError> {
        let body = RemoteWriteFileRequest {
            handle: Self::local_handle(handle),
            path: path.to_string(),
            contents_b64: base64::engine::general_purpose::STANDARD.encode(contents),
            mode,
        };
        let _: serde_json::Value = self
            .call(
                "/agent/sandboxes/write-file",
                &body,
                SHORT_TIMEOUT,
                Some(handle),
                "write_file",
            )
            .await?;
        Ok(())
    }

    async fn read_file(&self, handle: &SandboxHandle, path: &str) -> Result<Vec<u8>, AgentError> {
        let body = RemoteReadFileRequest {
            handle: Self::local_handle(handle),
            path: path.to_string(),
        };
        let r: RemoteFileContents = self
            .call(
                "/agent/sandboxes/read-file",
                &body,
                SHORT_TIMEOUT,
                Some(handle),
                "read_file",
            )
            .await?;
        base64::engine::general_purpose::STANDARD
            .decode(r.contents_b64)
            .map_err(|e| self.failed(Some(handle), "read_file", format!("invalid base64: {}", e)))
    }

    async fn write_directory(
        &self,
        handle: &SandboxHandle,
        local_dir: &std::path::Path,
        target_path: &str,
    ) -> Result<(), AgentError> {
        let dir = local_dir.to_path_buf();
        let archive = tokio::task::spawn_blocking(move || tar_directory(&dir))
            .await
            .map_err(|e| self.failed(Some(handle), "write_directory", e.to_string()))?
            .map_err(|e| {
                self.failed(
                    Some(handle),
                    "write_directory",
                    format!("archive {}: {}", local_dir.display(), e),
                )
            })?;
        let body = RemoteWriteDirectoryRequest {
            handle: Self::local_handle(handle),
            target_path: target_path.to_string(),
            tar_b64: base64::engine::general_purpose::STANDARD.encode(archive),
        };
        let _: serde_json::Value = self
            .call(
                "/agent/sandboxes/write-directory",
                &body,
                LIFECYCLE_TIMEOUT,
                Some(handle),
                "write_directory",
            )
            .await?;
        Ok(())
    }

    async fn kill_processes(
        &self,
        handle: &SandboxHandle,
        pattern: &str,
        signal: KillSignal,
    ) -> Result<(), AgentError> {
        let body = RemoteKillRequest {
            handle: Self::local_handle(handle),
            pattern: pattern.to_string(),
            signal,
        };
        let _: serde_json::Value = self
            .call(
                "/agent/sandboxes/kill-processes",
                &body,
                SHORT_TIMEOUT,
                Some(handle),
                "kill_processes",
            )
            .await?;
        Ok(())
    }

    async fn destroy(&self, handle: &SandboxHandle, purge_volumes: bool) -> Result<(), AgentError> {
        let body = RemoteDestroyRequest {
            handle: Self::local_handle(handle),
            purge_volumes,
        };
        let _: serde_json::Value = self
            .call(
                "/agent/sandboxes/destroy",
                &body,
                LIFECYCLE_TIMEOUT,
                Some(handle),
                "destroy",
            )
            .await?;
        Ok(())
    }

    async fn stop(&self, handle: &SandboxHandle) -> Result<(), AgentError> {
        let body = RemoteHandleRequest {
            handle: Self::local_handle(handle),
        };
        let _: serde_json::Value = self
            .call(
                "/agent/sandboxes/stop",
                &body,
                LIFECYCLE_TIMEOUT,
                Some(handle),
                "stop",
            )
            .await?;
        Ok(())
    }

    async fn start(&self, handle: &SandboxHandle) -> Result<(), AgentError> {
        let body = RemoteHandleRequest {
            handle: Self::local_handle(handle),
        };
        let _: serde_json::Value = self
            .call(
                "/agent/sandboxes/start",
                &body,
                LIFECYCLE_TIMEOUT,
                Some(handle),
                "start",
            )
            .await?;
        Ok(())
    }

    async fn recover(&self, _run_id: i32) -> Result<Option<SandboxHandle>, AgentError> {
        // Agent-run sandboxes are always local; standalone sandboxes recover
        // by name.
        Ok(None)
    }

    async fn recover_by_name(
        &self,
        container_name: &str,
    ) -> Result<Option<SandboxHandle>, AgentError> {
        let body = RemoteRecoverRequest {
            container_name: container_name.to_string(),
        };
        let handle: Option<SandboxHandle> = self
            .call(
                "/agent/sandboxes/recover",
                &body,
                SHORT_TIMEOUT,
                None,
                "recover",
            )
            .await?;
        Ok(handle.map(|h| self.stamp(h)))
    }

    fn supports_backend(&self, backend: SandboxBackend) -> bool {
        backend == SandboxBackend::Docker
    }

    async fn attach_pty(&self, handle: &SandboxHandle) -> Result<PtyAttachment, AgentError> {
        Err(self.unsupported(handle, "The interactive terminal"))
    }

    async fn take_snapshot(
        &self,
        handle: &SandboxHandle,
        _label: Option<String>,
        _max_size_bytes: u64,
    ) -> Result<SnapshotArtifact, AgentError> {
        Err(self.unsupported(handle, "Snapshots"))
    }

    async fn create_from_snapshot(
        &self,
        _artifact: &SnapshotArtifact,
        config: SandboxCreateConfig,
    ) -> Result<SandboxHandle, AgentError> {
        Err(AgentError::SandboxCreationFailed {
            run_id: config.run_id,
            provider: REMOTE_PROVIDER_NAME.into(),
            reason: format!(
                "restoring a snapshot is not available yet on worker nodes (requested node '{}'); \
                 restore it on the control plane",
                self.node_name
            ),
        })
    }

    async fn configure_application_network(
        &self,
        handle: &SandboxHandle,
        _network_name: &str,
        _service_containers: &[String],
    ) -> Result<(), AgentError> {
        Err(self.unsupported(handle, "Application service networking"))
    }

    async fn git_relay_base_url(
        &self,
        handle: &SandboxHandle,
        _control_plane_url: &str,
    ) -> Result<String, AgentError> {
        Err(self.unsupported(handle, "The git relay"))
    }

    // The relays below reach the control plane from inside the sandbox,
    // which is not verified from worker nodes yet (ADR-048 §6). Refuse
    // instead of handing out a URL the worker may not be able to reach.
    async fn model_relay_base_url(
        &self,
        handle: &SandboxHandle,
        _control_plane_url: &str,
    ) -> Result<String, AgentError> {
        Err(self.unsupported(handle, "The model relay"))
    }

    async fn harness_mcp_url(
        &self,
        handle: &SandboxHandle,
        _control_plane_url: &str,
        _registered_url: &str,
    ) -> Result<String, AgentError> {
        Err(self.unsupported(handle, "The harness MCP relay"))
    }

    async fn connect_agent_runtime(
        &self,
        handle: &SandboxHandle,
    ) -> Result<PtyAttachment, AgentError> {
        Err(self.unsupported(handle, "The retained agent runtime"))
    }

    async fn resize_disk(
        &self,
        handle: &SandboxHandle,
        _new_size_mb: u64,
    ) -> Result<(), AgentError> {
        Err(self.unsupported(handle, "Disk resize"))
    }

    fn name(&self) -> &str {
        REMOTE_PROVIDER_NAME
    }

    async fn is_available(&self) -> bool {
        let r: Result<RemoteStatusResponse, _> = self
            .call(
                "/agent/sandboxes/status",
                &serde_json::json!({}),
                SHORT_TIMEOUT,
                None,
                "status",
            )
            .await;
        r.map(|s| s.available).unwrap_or(false)
    }

    async fn image_status(&self) -> Result<(bool, String), AgentError> {
        let r: RemoteStatusResponse = self
            .call(
                "/agent/sandboxes/status",
                &serde_json::json!({}),
                SHORT_TIMEOUT,
                None,
                "status",
            )
            .await?;
        Ok((r.image_ready, r.image))
    }

    async fn rebuild_image(&self) -> Result<String, AgentError> {
        Err(AgentError::SandboxProviderUnavailable {
            provider: format!("node '{}'", self.node_name),
            reason: "rebuilding the sandbox image is only available on the control plane".into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_are_single_safe_path_segments() {
        assert!(is_valid_sandbox_label("abc123_XYZ-9"));
        assert!(!is_valid_sandbox_label(""));
        assert!(!is_valid_sandbox_label("../etc"));
        assert!(!is_valid_sandbox_label("a/b"));
        assert!(!is_valid_sandbox_label("a b"));
        assert!(!is_valid_sandbox_label(&"a".repeat(65)));
    }

    fn handle(name: &str) -> SandboxHandle {
        SandboxHandle {
            node_id: None,
            sandbox_id: "cid".into(),
            sandbox_name: name.into(),
            work_dir: "/workspace".into(),
            backend: SandboxBackend::Docker,
            image: String::new(),
        }
    }

    #[test]
    fn only_sandbox_containers_are_addressable() {
        assert!(is_sandbox_handle(&handle("temps-sandbox-abc123")));
        assert!(!is_sandbox_handle(&handle("temps-sandbox-")));
        assert!(!is_sandbox_handle(&handle("my-app-web-1")));
        assert!(!is_sandbox_handle(&handle("temps-sandbox-../x")));
    }

    #[test]
    fn handle_round_trips_with_node_stamp() {
        let mut h = handle("temps-sandbox-abc");
        h.node_id = Some(7);
        let json = serde_json::to_string(&h).unwrap();
        let back: SandboxHandle = serde_json::from_str(&json).unwrap();
        assert_eq!(back.node_id, Some(7));
        // Handles serialised before ADR-048 have no node_id → local.
        let legacy = r#"{"sandbox_id":"c","sandbox_name":"n","work_dir":"/w","backend":"docker","image":""}"#;
        let back: SandboxHandle = serde_json::from_str(legacy).unwrap();
        assert_eq!(back.node_id, None);
    }

    #[test]
    fn directory_archive_preserves_relative_paths() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub/a.txt"), b"hi").unwrap();
        let bytes = tar_directory(dir.path()).unwrap();
        let mut archive = tar::Archive::new(bytes.as_slice());
        let names: Vec<String> = archive
            .entries()
            .unwrap()
            .map(|e| e.unwrap().path().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(names.iter().any(|n| n.ends_with("sub/a.txt")), "{names:?}");
    }

    type Seen = std::sync::Arc<tokio::sync::Mutex<Option<serde_json::Value>>>;
    type Reply = std::sync::Arc<(u16, Option<serde_json::Value>)>;

    /// A fake worker agent answering every sandbox route with one scripted
    /// response, recording the last request body.
    async fn fake_agent(status: u16, body: Option<serde_json::Value>) -> (String, Seen) {
        use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
        let seen: Seen = Default::default();
        let reply: Reply = std::sync::Arc::new((status, body));
        let app =
            axum::Router::new()
                .fallback(
                    |State((seen, reply)): State<(Seen, Reply)>,
                     Json(req): Json<serde_json::Value>| async move {
                        *seen.lock().await = Some(req);
                        let status = StatusCode::from_u16(reply.0).unwrap();
                        match &reply.1 {
                            Some(body) => (status, Json(body.clone())).into_response(),
                            None => status.into_response(),
                        }
                    },
                )
                .with_state((seen.clone(), reply));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{addr}"), seen)
    }

    fn provider_at(url: &str) -> RemoteSandboxProvider {
        RemoteSandboxProvider::new(
            2,
            "worker-2".into(),
            url.into(),
            "tok".into(),
            reqwest::Client::new(),
        )
    }

    fn remote_handle() -> SandboxHandle {
        let mut h = handle("temps-sandbox-abc");
        h.node_id = Some(2);
        h
    }

    #[tokio::test]
    async fn node_level_statuses_mean_the_node_is_unavailable() {
        for status in [502, 503, 504] {
            let (url, _) = fake_agent(
                status,
                Some(serde_json::json!({"error": "no Docker daemon connection"})),
            )
            .await;
            let err = provider_at(&url)
                .is_alive(&remote_handle())
                .await
                .unwrap_err();
            match err {
                AgentError::SandboxProviderUnavailable { reason, .. } => {
                    assert!(reason.contains("worker-2"), "{reason}");
                    assert!(reason.contains("no Docker daemon"), "{reason}");
                }
                other => panic!("{status}: expected unavailable, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn bare_404_asks_to_upgrade_the_node() {
        let (url, _) = fake_agent(404, None).await;
        let err = provider_at(&url)
            .is_alive(&remote_handle())
            .await
            .unwrap_err();
        assert!(
            matches!(&err, AgentError::SandboxProviderUnavailable { reason, .. } if reason.contains("upgrade temps")),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn worker_errors_carry_the_worker_message() {
        let (url, _) = fake_agent(
            500,
            Some(serde_json::json!({"error": "docker exec failed: out of memory"})),
        )
        .await;
        let err = provider_at(&url)
            .read_file(&remote_handle(), "/x")
            .await
            .unwrap_err();
        assert!(
            matches!(&err, AgentError::SandboxExecFailed { reason, .. } if reason.contains("out of memory")),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn worker_statuses_map_to_the_matching_errors() {
        let body = || Some(serde_json::json!({"error": "detail"}));
        let (missing, _) = fake_agent(404, body()).await;
        let err = provider_at(&missing)
            .stop(&remote_handle())
            .await
            .unwrap_err();
        assert!(matches!(err, AgentError::SandboxNotFound { .. }), "{err:?}");

        let (invalid, _) = fake_agent(400, body()).await;
        let err = provider_at(&invalid)
            .stop(&remote_handle())
            .await
            .unwrap_err();
        assert!(matches!(err, AgentError::Validation { .. }), "{err:?}");

        let (refused, _) = fake_agent(401, body()).await;
        let err = provider_at(&refused)
            .stop(&remote_handle())
            .await
            .unwrap_err();
        assert!(
            matches!(&err, AgentError::SandboxProviderUnavailable { reason, .. } if reason.contains("re-join")),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn transport_failure_means_the_node_is_unavailable() {
        // Nothing listens on port 9 (discard) on loopback in the test env.
        let err = provider_at("http://127.0.0.1:9")
            .is_alive(&remote_handle())
            .await
            .unwrap_err();
        assert!(
            matches!(err, AgentError::SandboxProviderUnavailable { .. }),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn create_applies_the_operator_defaults_including_network_mode() {
        let created = serde_json::to_value(remote_handle()).unwrap();
        let (url, seen) = fake_agent(200, Some(created)).await;
        let provider = provider_at(&url).with_defaults(RemoteSandboxDefaults {
            image: Some("img:1".into()),
            cpu_limit: Some(1.5),
            memory_limit_mb: Some(512),
            network_mode: Some("none".into()),
        });
        let config = SandboxCreateConfig {
            node_id: Some(2),
            run_id: 1,
            container_name_override: Some("abc".into()),
            host_work_dir: std::path::PathBuf::from("/unused"),
            workspace_volume: None,
            image: None,
            cpu_limit: None,
            memory_limit_mb: None,
            pids_limit: None,
            disk_size_mb: None,
            network_mode: None,
            env_vars: Default::default(),
            idle_timeout: Duration::from_secs(60),
            backend: None,
            owner_user_id: None,
        };
        provider.create(config).await.unwrap();
        let body = seen.lock().await.clone().unwrap();
        assert_eq!(body["network_mode"], "none");
        assert_eq!(body["image"], "img:1");
        assert_eq!(body["memory_limit_mb"], 512);
    }
}
