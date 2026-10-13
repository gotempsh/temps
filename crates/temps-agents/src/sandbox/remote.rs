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
//! Phase-1 limits, each surfaced as an explicit
//! [`AgentError::SandboxUnsupportedOnNode`] instead of a silent fallback:
//! interactive terminals, the retained agent runtime, snapshots, disk resize,
//! workspace volumes, the Firecracker backend and application networks are
//! not available for sandboxes on worker nodes yet.
//!
//! Exec output: an exec with a line callback (`exec_streamed`, and the other
//! exec variants when given a callback) goes through
//! `/agent/sandboxes/exec-stream`, which answers newline-delimited JSON
//! [`RemoteExecFrame`]s as the command produces output, so callbacks see
//! lines live, as they do for a local sandbox. That call has no total
//! timeout — a dev server may run for days — only an idle timeout: the
//! worker sends a heartbeat frame while the command is quiet, and a stream
//! that stays silent for longer than [`EXEC_STREAM_IDLE_TIMEOUT`] means the
//! node is gone. An exec without a callback keeps the single-response
//! `/agent/sandboxes/exec` call.
//!
//! Trust boundary: a worker node is less trusted than the control plane.
//! Everything read back from one is bounded — response bodies are read with
//! a byte cap per operation, and worker-written error text is sanitised
//! (control characters dropped, length capped) before it can reach an API
//! client or a terminal. Transport errors never echo the node's internal URL.

use async_trait::async_trait;
use base64::Engine;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;

use super::{
    ExecStream, KillSignal, OnStreamEventCallback, PtyAttachment, RuntimeCompatibility,
    SandboxBackend, SandboxCreateConfig, SandboxExecResult, SandboxHandle, SandboxProvider,
    SnapshotArtifact,
};
use crate::ai_cli::OnEventCallback;
use crate::error::AgentError;

/// Name reported by [`RemoteSandboxProvider::name`].
pub const REMOTE_PROVIDER_NAME: &str = "remote";

/// Timeout for lifecycle calls (create pulls the image on first use).
const LIFECYCLE_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// Timeout for a single-response `exec` (no line callback). Matches the
/// longest command the sandbox API accepts. Streamed execs have no total
/// timeout, only [`EXEC_STREAM_IDLE_TIMEOUT`].
const EXEC_TIMEOUT: Duration = Duration::from_secs(60 * 60);
/// Timeout for cheap calls (liveness, file IO, recovery lookups).
const SHORT_TIMEOUT: Duration = Duration::from_secs(120);
/// How long the worker waits on a quiet command before it sends a
/// [`RemoteExecFrame::Heartbeat`], so the control plane can tell a quiet
/// command from a dead node.
pub const EXEC_STREAM_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);
/// Longest silence (no frame at all, heartbeats included) the control plane
/// accepts on an exec stream before it reports the node unavailable. Four
/// missed heartbeats.
const EXEC_STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(60);

// ── Size limits shared by both ends ─────────────────────────────────────

/// Most exec output (per stream) a worker returns to the control plane. The
/// worker keeps the tail of anything longer, so one noisy command cannot
/// make the response unbounded.
pub const WORKER_EXEC_OUTPUT_LIMIT: usize = 16 * 1024 * 1024;

/// Largest file a worker reads out of a sandbox for the control plane. The
/// worker refuses bigger files before buffering them (a 400 naming the
/// sandbox, path and this limit). Local sandboxes have no such limit.
pub const WORKER_READ_FILE_MAX_BYTES: u64 = 100 * 1024 * 1024;

/// Largest error body read from a worker. Error bodies are one short message.
const ERROR_BODY_CAP: usize = 16 * 1024;

/// Cap for responses that carry a few fields (handles, status, `{ok}`).
const SMALL_RESPONSE_CAP: usize = 1024 * 1024;

/// Cap for an exec response: two streams of up to
/// [`WORKER_EXEC_OUTPUT_LIMIT`] bytes each, plus their truncation markers.
/// JSON escapes a control byte as `\u00XX` (6 bytes), so the worst case is
/// six times the raw output.
const EXEC_RESPONSE_CAP: usize = 2 * 6 * WORKER_EXEC_OUTPUT_LIMIT + 1024 * 1024;

/// Cap for a read-file response: the base64 of a
/// [`WORKER_READ_FILE_MAX_BYTES`] file (4/3 overhead) plus the JSON frame.
const READ_FILE_RESPONSE_CAP: usize =
    (WORKER_READ_FILE_MAX_BYTES as usize).div_ceil(3) * 4 + 1024 * 1024;

/// Most output bytes a worker puts in one exec-stream frame. A longer line
/// is sent as several continuation frames (see [`RemoteExecFrame`]), so one
/// frame — and the control plane's frame buffer — stays bounded without
/// cutting the line.
pub const WORKER_EXEC_LINE_LIMIT: usize = 64 * 1024;

/// Most bytes the control plane buffers for one exec-stream frame: a
/// [`WORKER_EXEC_LINE_LIMIT`] line of control bytes (each escaped as
/// `\u00XX`, 6 bytes) plus the truncation marker and the JSON frame.
const EXEC_FRAME_CAP: usize = 6 * WORKER_EXEC_LINE_LIMIT + 4 * 1024;

/// Longest worker-written message kept in an error. Same bound the sandbox
/// service applies to eviction reasons.
pub const MAX_WORKER_MESSAGE_CHARS: usize = 512;

// The caps must fit everything a worker may legitimately send: two full
// exec streams of control bytes (each escaped as `\u00XX`), the base64 of
// the largest readable file, and a full-length error message.
const _: () = {
    assert!(EXEC_RESPONSE_CAP > 2 * 6 * WORKER_EXEC_OUTPUT_LIMIT);
    assert!(READ_FILE_RESPONSE_CAP > (WORKER_READ_FILE_MAX_BYTES as usize).div_ceil(3) * 4);
    assert!(ERROR_BODY_CAP >= 4 * MAX_WORKER_MESSAGE_CHARS);
    assert!(EXEC_FRAME_CAP > 6 * WORKER_EXEC_LINE_LIMIT + 256);
    // A quiet command must never look like a dead node.
    assert!(EXEC_STREAM_IDLE_TIMEOUT.as_secs() >= 3 * EXEC_STREAM_HEARTBEAT_INTERVAL.as_secs());
};

/// Make text written by a worker node safe to put in an error that reaches
/// an API client or a terminal: control characters (ESC included, so no
/// terminal escape sequences) become spaces, and the text is capped at
/// [`MAX_WORKER_MESSAGE_CHARS`] characters, marked with `…` when cut.
pub fn sanitize_worker_message(message: &str) -> String {
    let mut out: String = message
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(MAX_WORKER_MESSAGE_CHARS)
        .collect();
    if message.chars().count() > MAX_WORKER_MESSAGE_CHARS {
        out.push('…');
    }
    out
}

// ── Wire types ──────────────────────────────────────────────────────────

/// `POST /agent/sandboxes` — create a sandbox on the worker.
///
/// There is deliberately no host path in this request: the worker derives
/// the sandbox work directory from `label` under its own data directory, so
/// the control plane can never ask a worker to bind-mount an arbitrary path.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
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

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct RemoteHandleRequest {
    pub handle: SandboxHandle,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct RemoteDestroyRequest {
    pub handle: SandboxHandle,
    pub purge_volumes: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct RemoteExecRequest {
    pub handle: SandboxHandle,
    pub cmd: Vec<String>,
    pub env: HashMap<String, String>,
    /// Run as this user (`exec_as_user`). Ignored when `as_root` is set.
    pub user: Option<String>,
    pub as_root: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct RemoteExecResponse {
    pub exit_code: i32,
    /// Standard output; at most [`WORKER_EXEC_OUTPUT_LIMIT`] bytes (the
    /// tail is kept, with a marker saying how much was dropped).
    pub stdout: String,
    /// Standard error, bounded like `stdout`.
    pub stderr: String,
}

/// One newline-delimited JSON frame of `POST /agent/sandboxes/exec-stream`.
///
/// The stream carries output lines as the command produces them, heartbeats
/// while it is quiet, and ends with exactly one `exit` or `error` frame. A
/// stream that ends without either was cut short (the worker or its agent
/// went away).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RemoteExecFrame {
    /// Standard output: one line without its newline, or one piece of it.
    /// A frame carries at most [`WORKER_EXEC_LINE_LIMIT`] bytes, so a longer
    /// line is sent as several frames, all but the last with `more` set,
    /// and rebuilt on the control plane.
    Stdout {
        line: String,
        /// The next `stdout` frame continues this line.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        more: bool,
    },
    /// Standard error, framed like `stdout`.
    Stderr {
        line: String,
        /// The next `stderr` frame continues this line.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        more: bool,
    },
    /// The command is still running and has been quiet for
    /// [`EXEC_STREAM_HEARTBEAT_INTERVAL`].
    Heartbeat,
    /// The command finished with this exit code (any value; a non-zero exit
    /// is not an error).
    Exit { exit_code: i32 },
    /// The exec failed on the worker after the stream started. `status` is
    /// the HTTP status the same failure gets on the single-response routes,
    /// so the control plane maps both the same way.
    Error { status: u16, error: String },
}

impl RemoteExecFrame {
    /// The output frames for one `line`: a single frame, or pieces of at
    /// most [`WORKER_EXEC_LINE_LIMIT`] bytes split on character boundaries,
    /// every piece but the last marked `more`. Nothing is dropped here; the
    /// control plane caps a rebuilt line at [`WORKER_EXEC_OUTPUT_LIMIT`].
    pub fn output_frames(stream: ExecStream, line: String) -> Vec<Self> {
        let frame = |line: String, more: bool| match stream {
            ExecStream::Stdout => Self::Stdout { line, more },
            ExecStream::Stderr => Self::Stderr { line, more },
        };
        if line.len() <= WORKER_EXEC_LINE_LIMIT {
            return vec![frame(line, false)];
        }
        let mut frames = Vec::with_capacity(line.len().div_ceil(WORKER_EXEC_LINE_LIMIT));
        let mut rest = line.as_str();
        while rest.len() > WORKER_EXEC_LINE_LIMIT {
            let mut end = WORKER_EXEC_LINE_LIMIT;
            while !rest.is_char_boundary(end) {
                end -= 1;
            }
            frames.push(frame(rest[..end].to_string(), true));
            rest = &rest[end..];
        }
        frames.push(frame(rest.to_string(), false));
        frames
    }
}

/// Rebuilds one stream's lines from exec-stream frames, keeping a line to
/// at most `limit` bytes (the per-stream output limit) however many
/// continuation frames a worker sends.
struct LineAssembler {
    text: String,
    dropped: usize,
    limit: usize,
}

impl LineAssembler {
    fn new(limit: usize) -> Self {
        Self {
            text: String::new(),
            dropped: 0,
            limit,
        }
    }

    /// Add one frame's piece; returns the whole line once `more` is false.
    fn push(&mut self, piece: &str, more: bool) -> Option<String> {
        let room = self.limit.saturating_sub(self.text.len());
        if self.dropped > 0 {
            // The line was already cut: keep it the line's start, never the
            // start plus scraps from further on.
            self.dropped += piece.len();
        } else if piece.len() <= room {
            self.text.push_str(piece);
        } else {
            let mut end = room;
            while !piece.is_char_boundary(end) {
                end -= 1;
            }
            self.text.push_str(&piece[..end]);
            self.dropped += piece.len() - end;
        }
        if more {
            return None;
        }
        let mut line = std::mem::take(&mut self.text);
        if self.dropped > 0 {
            line.push_str(&format!(
                " [{} more bytes of this line truncated on the control plane]",
                self.dropped
            ));
            self.dropped = 0;
        }
        Some(line)
    }

    /// A line the stream ended in the middle of.
    fn flush(&mut self) -> Option<String> {
        (!self.text.is_empty() || self.dropped > 0).then(|| self.push("", false))?
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct RemoteReadFileRequest {
    pub handle: SandboxHandle,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct RemoteFileContents {
    /// Base64 (standard alphabet) file bytes; the file is at most
    /// [`WORKER_READ_FILE_MAX_BYTES`].
    pub contents_b64: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct RemoteWriteFileRequest {
    pub handle: SandboxHandle,
    pub path: String,
    pub contents_b64: String,
    pub mode: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct RemoteWriteDirectoryRequest {
    pub handle: SandboxHandle,
    pub target_path: String,
    /// Base64 tar archive of the directory contents (paths relative to the
    /// directory root), as built by [`tar_directory`]. The worker accepts
    /// only regular-file and directory entries with relative paths.
    pub tar_b64: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct RemoteKillRequest {
    pub handle: SandboxHandle,
    pub pattern: String,
    pub signal: KillSignal,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct RemoteRecoverRequest {
    /// Sandbox label, without the `temps-sandbox-` prefix.
    pub container_name: String,
}

/// `POST /agent/sandboxes/status` takes an empty JSON object.
#[derive(Debug, Clone, Default, Serialize, Deserialize, utoipa::ToSchema)]
pub struct RemoteStatusRequest {}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct RemoteAliveResponse {
    pub alive: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct RemoteStatusResponse {
    pub available: bool,
    pub image_ready: bool,
    pub image: String,
}

/// Body of a successful call that returns nothing else.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct RemoteOkResponse {
    pub ok: bool,
}

/// Error body returned by every `/agent/sandboxes/*` endpoint on failure.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct RemoteErrorBody {
    pub error: String,
}

/// Container-name prefix every sandbox container carries. The worker only
/// operates on handles whose name has this prefix *and* whose container
/// carries [`super::docker::SANDBOX_CONTAINER_LABEL`], so the sandbox API can
/// never be pointed at an application, service or sidecar container.
pub const SANDBOX_CONTAINER_PREFIX: &str = "temps-sandbox-";

/// Label prefix reserved for the sandbox egress proxy sidecar
/// (`temps-sandbox-egress-proxy-v2-…`); no sandbox may use it.
const RESERVED_LABEL_PREFIX: &str = "egress-proxy";

/// A sandbox label is the container suffix and the name of the worker-side
/// work directory, so it must be a single safe path segment. Labels that
/// would make the container name collide with the egress proxy sidecar's
/// are refused.
pub fn is_valid_sandbox_label(label: &str) -> bool {
    !label.is_empty()
        && label.len() <= 64
        && !label.starts_with(RESERVED_LABEL_PREFIX)
        && label
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// True when `handle` names a sandbox container this API may operate on.
/// A name check only: the worker also requires the sandbox label on the
/// container it resolves.
pub fn is_sandbox_handle(handle: &SandboxHandle) -> bool {
    handle
        .sandbox_name
        .strip_prefix(SANDBOX_CONTAINER_PREFIX)
        .is_some_and(is_valid_sandbox_label)
}

/// Build the `write-directory` archive for `dir` in memory: one entry per
/// regular file, with its path relative to `dir`. Symlinks are resolved on
/// this side and only while they stay inside `dir` (the same rule the local
/// Docker provider applies), so the worker receives plain files only.
pub fn tar_directory(dir: &std::path::Path) -> Result<Vec<u8>, std::io::Error> {
    // `dir` is its own containment root: callers whose links may reach a
    // wider tree stage it first (`docker::stage_directory_upload`).
    let files = super::docker::directory_upload_files(dir, dir)?.files;
    let mut builder = tar::Builder::new(Vec::new());
    builder.mode(tar::HeaderMode::Deterministic);
    for (path, relative) in files {
        builder.append_path_with_name(&path, &relative)?;
    }
    builder.into_inner()
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
    /// See [`EXEC_STREAM_IDLE_TIMEOUT`]; shortened by tests.
    exec_stream_idle_timeout: Duration,
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

/// Why a response body could not be read within its cap.
enum BodyReadError {
    TooLarge,
    Transport(reqwest::Error),
}

/// Read `response`'s body, giving up as soon as it exceeds `cap` bytes, so a
/// misbehaving worker cannot make the control plane buffer without bound.
async fn read_body_capped(
    mut response: reqwest::Response,
    cap: usize,
) -> Result<Vec<u8>, BodyReadError> {
    if response
        .content_length()
        .is_some_and(|declared| declared > cap as u64)
    {
        return Err(BodyReadError::TooLarge);
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(BodyReadError::Transport)? {
        if body.len() + chunk.len() > cap {
            return Err(BodyReadError::TooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// What a transport error was, without the URL (which names the node's
/// internal address and port) or anything else from the error's text.
fn describe_transport_error(error: &reqwest::Error) -> &'static str {
    if error.is_timeout() {
        "the request to the node's agent timed out"
    } else if error.is_connect() {
        "could not connect to the node's agent"
    } else if error.is_body() || error.is_decode() {
        "the connection to the node's agent failed while reading its answer"
    } else if error.is_redirect() {
        "the node's agent answered with a redirect"
    } else {
        "the request to the node's agent failed"
    }
}

/// The last `limit` bytes of one exec stream, rebuilt from its lines, for
/// the [`SandboxExecResult`] an exec stream returns. Trimmed in batches (at
/// a quarter over the limit) so a long-running command costs amortised O(1)
/// per byte and at most 1.25 × `limit` of memory.
struct OutputTail {
    text: String,
    dropped: usize,
    limit: usize,
}

impl OutputTail {
    fn new(limit: usize) -> Self {
        Self {
            text: String::new(),
            dropped: 0,
            limit,
        }
    }

    fn push_line(&mut self, line: &str) {
        self.text.push_str(line);
        self.text.push('\n');
        if self.text.len() > self.limit + self.limit / 4 {
            self.trim();
        }
    }

    fn trim(&mut self) {
        if self.text.len() <= self.limit {
            return;
        }
        let mut start = self.text.len() - self.limit;
        while !self.text.is_char_boundary(start) {
            start += 1;
        }
        self.text.drain(..start);
        self.dropped += start;
    }

    /// The kept tail, marked like the worker marks a truncated exec response.
    fn finish(mut self) -> String {
        self.trim();
        if self.dropped == 0 {
            self.text
        } else {
            format!(
                "[{} earlier bytes truncated on the control plane]\n{}",
                self.dropped, self.text
            )
        }
    }
}

/// Forward only stdout lines to a stdout-only callback, the way the local
/// provider's `exec`/`exec_as_root`/`exec_as_user` treat their callback.
fn stdout_only(cb: OnEventCallback) -> OnStreamEventCallback {
    std::sync::Arc::new(move |stream: ExecStream, line: String| {
        let cb = cb.clone();
        let fut: std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> =
            Box::pin(async move {
                if stream == ExecStream::Stdout {
                    cb(line).await;
                }
            });
        fut
    })
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
            exec_stream_idle_timeout: EXEC_STREAM_IDLE_TIMEOUT,
        }
    }

    pub fn with_defaults(mut self, defaults: RemoteSandboxDefaults) -> Self {
        self.defaults = defaults;
        self
    }

    #[cfg(test)]
    fn with_exec_stream_idle_timeout(mut self, timeout: Duration) -> Self {
        self.exec_stream_idle_timeout = timeout;
        self
    }

    pub fn node_id(&self) -> i32 {
        self.node_id
    }

    pub fn node_name(&self) -> &str {
        &self.node_name
    }

    /// The node cannot serve the request. `reason` must already be safe to
    /// show a sandbox owner: no internal addresses, worker text sanitised.
    fn unavailable(&self, reason: impl std::fmt::Display) -> AgentError {
        AgentError::SandboxProviderUnavailable {
            provider: format!("node '{}'", self.node_name),
            reason: format!(
                "worker node '{}' (id {}) is unavailable: {}",
                self.node_name, self.node_id, reason
            ),
        }
    }

    /// A transport failure. The full error (with the agent URL) is logged
    /// for the operator; the returned error only names the node and the
    /// kind of failure, because it can reach non-admin sandbox owners.
    fn transport_unavailable(&self, operation: &str, error: reqwest::Error) -> AgentError {
        tracing::warn!(
            node_id = self.node_id,
            node_name = %self.node_name,
            operation,
            error = %error,
            "Sandbox call to worker node failed in transport"
        );
        self.unavailable(format!(
            "{} ({})",
            describe_transport_error(&error),
            operation
        ))
    }

    /// The operation failed on the worker. Remote handles carry no agent
    /// run id, so `run_id` is 0; the sandbox, node and operation are in the
    /// error itself.
    fn failed(
        &self,
        handle: Option<&SandboxHandle>,
        operation: &str,
        reason: String,
    ) -> AgentError {
        AgentError::SandboxExecFailed {
            run_id: 0,
            sandbox_id: handle.map(|h| h.sandbox_name.clone()).unwrap_or_default(),
            reason: format!(
                "{} on worker node '{}' failed: {}",
                operation, self.node_name, reason
            ),
        }
    }

    /// A feature that only works for sandboxes on the control plane yet.
    /// `sandbox` is the container name (or label, before one exists); empty
    /// for node-level features.
    fn unsupported(&self, sandbox: &str, feature: &str) -> AgentError {
        AgentError::SandboxUnsupportedOnNode {
            sandbox_id: sandbox.to_string(),
            node_name: self.node_name.clone(),
            feature: feature.to_string(),
        }
    }

    /// POST `body` to `path`, returning the decoded JSON response, read with
    /// at most `response_cap` bytes. Transport failures and 502/503/504 (the
    /// node, not the request, is the problem — e.g. its Docker daemon is
    /// down) become `SandboxProviderUnavailable`; any other non-2xx answer
    /// carries the worker's (sanitised) error message.
    async fn call<B: Serialize + ?Sized, R: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
        timeout: Duration,
        handle: Option<&SandboxHandle>,
        operation: &str,
        response_cap: usize,
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
            .map_err(|e| self.transport_unavailable(operation, e))?;
        if !response.status().is_success() {
            return Err(self.error_response(response, handle, operation).await);
        }
        let bytes = match read_body_capped(response, response_cap).await {
            Ok(bytes) => bytes,
            Err(BodyReadError::TooLarge) => {
                tracing::warn!(
                    node_id = self.node_id,
                    node_name = %self.node_name,
                    operation,
                    limit_bytes = response_cap,
                    "Worker node response exceeded its size limit"
                );
                return Err(self.failed(
                    handle,
                    operation,
                    format!(
                        "the node's response exceeded the {} byte limit for {}",
                        response_cap, operation
                    ),
                ));
            }
            Err(BodyReadError::Transport(e)) => {
                return Err(self.transport_unavailable(operation, e));
            }
        };
        serde_json::from_slice::<R>(&bytes).map_err(|e| {
            self.failed(
                handle,
                operation,
                format!("the node answered with an invalid response: {}", e),
            )
        })
    }

    /// Read a non-2xx answer's (capped) error body and map it to the error
    /// callers act on.
    async fn error_response(
        &self,
        response: reqwest::Response,
        handle: Option<&SandboxHandle>,
        operation: &str,
    ) -> AgentError {
        let status = response.status();
        let message = match read_body_capped(response, ERROR_BODY_CAP).await {
            Ok(bytes) => serde_json::from_slice::<RemoteErrorBody>(&bytes)
                .ok()
                .map(|b| sanitize_worker_message(&b.error)),
            Err(BodyReadError::TooLarge) => Some(format!(
                "HTTP {} with an error body over the {} byte limit (discarded)",
                status.as_u16(),
                ERROR_BODY_CAP
            )),
            Err(BodyReadError::Transport(e)) => {
                tracing::warn!(
                    node_id = self.node_id,
                    node_name = %self.node_name,
                    operation,
                    status = status.as_u16(),
                    error = %e,
                    "Failed to read a worker node's error body"
                );
                None
            }
        };
        self.status_error(status, message, handle, operation)
    }

    /// Map a non-2xx answer to the error callers act on.
    fn status_error(
        &self,
        status: reqwest::StatusCode,
        message: Option<String>,
        handle: Option<&SandboxHandle>,
        operation: &str,
    ) -> AgentError {
        match (status, message) {
            (
                reqwest::StatusCode::BAD_GATEWAY
                | reqwest::StatusCode::SERVICE_UNAVAILABLE
                | reqwest::StatusCode::GATEWAY_TIMEOUT,
                message,
            ) => self.unavailable(message.unwrap_or_else(|| format!("HTTP {}", status.as_u16()))),
            // The node refused our credentials: nothing on this side can
            // fix that, the node has to be enrolled again.
            (reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN, _) => self
                .unavailable(
                    "the node rejected the control plane's credentials; \
                     re-join it with `temps join`",
                ),
            // The worker reports a missing container with an error body;
            // callers treat that as "already gone", as they do locally.
            // `SandboxNotFound` is what every caller matches for that, so it
            // is kept here even though a remote handle has no run id; the
            // container and node are logged.
            (reqwest::StatusCode::NOT_FOUND, Some(_)) => {
                tracing::debug!(
                    node_id = self.node_id,
                    node_name = %self.node_name,
                    operation,
                    sandbox = handle.map(|h| h.sandbox_name.as_str()).unwrap_or_default(),
                    "Sandbox container does not exist on its worker node"
                );
                AgentError::SandboxNotFound {
                    run_id: 0,
                    sandbox: format!(
                        "{} on worker node '{}'",
                        handle
                            .map(|h| h.sandbox_name.as_str())
                            .unwrap_or("container"),
                        self.node_name
                    ),
                }
            }
            (reqwest::StatusCode::BAD_REQUEST, Some(message)) => AgentError::Validation {
                message: format!(
                    "{} on worker node '{}': {}",
                    operation, self.node_name, message
                ),
            },
            // The worker refused to replace a live sandbox (create). The
            // caller must not clean up: the conflicting sandbox isn't its own.
            (reqwest::StatusCode::CONFLICT, message) => AgentError::SandboxConflictOnNode {
                sandbox: handle
                    .map(|h| h.sandbox_name.clone())
                    .unwrap_or_else(|| operation.to_string()),
                node_name: self.node_name.clone(),
                reason: message.unwrap_or_else(|| "HTTP 409".to_string()),
            },
            // The worker can't serve this operation for its sandboxes.
            (reqwest::StatusCode::UNPROCESSABLE_ENTITY, message) => {
                tracing::debug!(
                    node_id = self.node_id,
                    node_name = %self.node_name,
                    operation,
                    message = message.as_deref().unwrap_or_default(),
                    "Worker node does not support a sandbox operation"
                );
                self.unsupported(
                    handle.map(|h| h.sandbox_name.as_str()).unwrap_or_default(),
                    operation,
                )
            }
            (_, Some(message)) => self.failed(handle, operation, message),
            // A worker agent older than ADR-048 has no sandbox routes, so
            // axum answers a bare 404 with no error body.
            (reqwest::StatusCode::NOT_FOUND, None) => self.unavailable(
                "the agent on this node does not support sandboxes; upgrade temps on the node",
            ),
            (_, None) => self.failed(handle, operation, format!("HTTP {}", status.as_u16())),
        }
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

    /// A handle-based call whose answer is just `{ok}`.
    async fn call_ok<B: Serialize + ?Sized>(
        &self,
        path: &str,
        body: &B,
        timeout: Duration,
        handle: &SandboxHandle,
        operation: &str,
    ) -> Result<(), AgentError> {
        let _: RemoteOkResponse = self
            .call(
                path,
                body,
                timeout,
                Some(handle),
                operation,
                SMALL_RESPONSE_CAP,
            )
            .await?;
        Ok(())
    }

    async fn status(&self) -> Result<RemoteStatusResponse, AgentError> {
        self.call(
            "/agent/sandboxes/status",
            &RemoteStatusRequest {},
            SHORT_TIMEOUT,
            None,
            "status",
            SMALL_RESPONSE_CAP,
        )
        .await
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
                EXEC_RESPONSE_CAP,
            )
            .await?;
        Ok(SandboxExecResult {
            exit_code: r.exit_code,
            stdout: r.stdout,
            stderr: r.stderr,
        })
    }

    /// Run a command through `/agent/sandboxes/exec-stream`, handing every
    /// output line to `on_event` as the worker sends it, and return when the
    /// command exits. There is no total timeout (a dev server may run for
    /// days); a stream silent for longer than the idle timeout — heartbeats
    /// included — means the node is gone. Dropping the returned future
    /// closes the connection, which stops the exec on the worker the same
    /// way dropping a local exec does (the process itself is stopped with
    /// `kill_processes`).
    async fn run_exec_streamed(
        &self,
        handle: &SandboxHandle,
        cmd: Vec<String>,
        env: HashMap<String, String>,
        user: Option<String>,
        as_root: bool,
        on_event: Option<OnStreamEventCallback>,
    ) -> Result<SandboxExecResult, AgentError> {
        const OPERATION: &str = "exec";
        let body = RemoteExecRequest {
            handle: Self::local_handle(handle),
            cmd,
            env,
            user,
            as_root,
        };
        let url = format!("{}/agent/sandboxes/exec-stream", self.agent_url);
        // Only the wait for the response head is bounded here: the worker
        // answers it once it has resolved the sandbox, before the command
        // produces anything.
        let response = tokio::time::timeout(
            SHORT_TIMEOUT,
            self.client
                .post(&url)
                .bearer_auth(&self.token)
                .json(&body)
                .send(),
        )
        .await
        .map_err(|_| {
            tracing::warn!(
                node_id = self.node_id,
                node_name = %self.node_name,
                sandbox = %handle.sandbox_name,
                timeout_secs = SHORT_TIMEOUT.as_secs(),
                "Worker node did not start a streamed sandbox exec in time"
            );
            self.unavailable(format!(
                "the node's agent did not start the command within {}s ({})",
                SHORT_TIMEOUT.as_secs(),
                OPERATION
            ))
        })?
        .map_err(|e| self.transport_unavailable(OPERATION, e))?;
        if !response.status().is_success() {
            return Err(self.error_response(response, Some(handle), OPERATION).await);
        }
        self.read_exec_stream(response, handle, on_event).await
    }

    /// Consume an exec-stream body: see [`RemoteExecFrame`].
    async fn read_exec_stream(
        &self,
        mut response: reqwest::Response,
        handle: &SandboxHandle,
        on_event: Option<OnStreamEventCallback>,
    ) -> Result<SandboxExecResult, AgentError> {
        const OPERATION: &str = "exec";
        let idle = self.exec_stream_idle_timeout;
        let mut pending: Vec<u8> = Vec::new();
        let mut stdout = OutputTail::new(WORKER_EXEC_OUTPUT_LIMIT);
        let mut stderr = OutputTail::new(WORKER_EXEC_OUTPUT_LIMIT);
        let mut stdout_line = LineAssembler::new(WORKER_EXEC_OUTPUT_LIMIT);
        let mut stderr_line = LineAssembler::new(WORKER_EXEC_OUTPUT_LIMIT);
        loop {
            let chunk = match tokio::time::timeout(idle, response.chunk()).await {
                Ok(Ok(Some(chunk))) => chunk,
                Ok(Ok(None)) => {
                    tracing::warn!(
                        node_id = self.node_id,
                        node_name = %self.node_name,
                        sandbox = %handle.sandbox_name,
                        "Worker node ended a sandbox exec stream without an exit status"
                    );
                    return Err(self.unavailable(format!(
                        "the node's agent closed the output stream of sandbox '{}' before \
                         the command finished ({})",
                        handle.sandbox_name, OPERATION
                    )));
                }
                Ok(Err(e)) => return Err(self.transport_unavailable(OPERATION, e)),
                Err(_) => {
                    tracing::warn!(
                        node_id = self.node_id,
                        node_name = %self.node_name,
                        sandbox = %handle.sandbox_name,
                        idle_secs = idle.as_secs(),
                        "Worker node sent nothing on a sandbox exec stream, not even a heartbeat"
                    );
                    return Err(self.unavailable(format!(
                        "the node's agent sent nothing for {}s while a command ran in sandbox \
                         '{}' ({}); the command may still be running there",
                        idle.as_secs(),
                        handle.sandbox_name,
                        OPERATION
                    )));
                }
            };
            pending.extend_from_slice(&chunk);
            while let Some(end) = pending.iter().position(|b| *b == b'\n') {
                // Checked before the frame is parsed or handed on: a frame
                // that arrives whole with its newline must not get past the
                // bound that only partial frames would otherwise meet.
                if end > EXEC_FRAME_CAP {
                    return Err(self.oversized_exec_frame(handle));
                }
                let frame_bytes: Vec<u8> = pending.drain(..=end).collect();
                let frame_bytes = &frame_bytes[..end];
                if frame_bytes.iter().all(u8::is_ascii_whitespace) {
                    continue;
                }
                let frame: RemoteExecFrame = serde_json::from_slice(frame_bytes).map_err(|e| {
                    self.failed(
                        Some(handle),
                        OPERATION,
                        format!("the node sent an invalid exec frame: {}", e),
                    )
                })?;
                match frame {
                    RemoteExecFrame::Stdout { line, more } => {
                        if let Some(line) = stdout_line.push(&line, more) {
                            stdout.push_line(&line);
                            if let Some(cb) = &on_event {
                                cb(ExecStream::Stdout, line).await;
                            }
                        }
                    }
                    RemoteExecFrame::Stderr { line, more } => {
                        if let Some(line) = stderr_line.push(&line, more) {
                            stderr.push_line(&line);
                            if let Some(cb) = &on_event {
                                cb(ExecStream::Stderr, line).await;
                            }
                        }
                    }
                    RemoteExecFrame::Heartbeat => {}
                    RemoteExecFrame::Exit { exit_code } => {
                        for (stream, assembler, tail) in [
                            (ExecStream::Stdout, &mut stdout_line, &mut stdout),
                            (ExecStream::Stderr, &mut stderr_line, &mut stderr),
                        ] {
                            if let Some(line) = assembler.flush() {
                                tail.push_line(&line);
                                if let Some(cb) = &on_event {
                                    cb(stream, line).await;
                                }
                            }
                        }
                        return Ok(SandboxExecResult {
                            exit_code,
                            stdout: stdout.finish(),
                            stderr: stderr.finish(),
                        });
                    }
                    RemoteExecFrame::Error { status, error } => {
                        // A success status can't describe a failure; treat
                        // anything unusable as "the operation failed".
                        let status = reqwest::StatusCode::from_u16(status)
                            .ok()
                            .filter(|s| !s.is_success())
                            .unwrap_or(reqwest::StatusCode::INTERNAL_SERVER_ERROR);
                        return Err(self.status_error(
                            status,
                            Some(sanitize_worker_message(&error)),
                            Some(handle),
                            OPERATION,
                        ));
                    }
                }
            }
            if pending.len() > EXEC_FRAME_CAP {
                return Err(self.oversized_exec_frame(handle));
            }
        }
    }

    fn oversized_exec_frame(&self, handle: &SandboxHandle) -> AgentError {
        tracing::warn!(
            node_id = self.node_id,
            node_name = %self.node_name,
            sandbox = %handle.sandbox_name,
            limit_bytes = EXEC_FRAME_CAP,
            "Worker node sent an exec frame over its size limit"
        );
        self.failed(
            Some(handle),
            "exec",
            format!(
                "the node sent an exec frame over the {} byte limit",
                EXEC_FRAME_CAP
            ),
        )
    }
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
            return Err(self.unsupported(&label, "Workspace volumes"));
        }
        if config.backend == Some(SandboxBackend::Firecracker) {
            return Err(self.unsupported(&label, "The firecracker backend"));
        }
        let body = RemoteCreateRequest {
            run_id: config.run_id,
            label,
            // An empty image means "not set", as on the control plane's own
            // provider; forwarding "" would let the worker fall back to its
            // built-in default instead of the operator's configured image.
            image: config
                .image
                .filter(|image| !image.is_empty())
                .or_else(|| self.defaults.image.clone()),
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
            .call(
                "/agent/sandboxes",
                &body,
                LIFECYCLE_TIMEOUT,
                None,
                "create",
                SMALL_RESPONSE_CAP,
            )
            .await
            .map_err(|e| match e {
                AgentError::SandboxExecFailed { reason, .. } => AgentError::SandboxCreationFailed {
                    run_id: config.run_id,
                    provider: format!("{} (node '{}')", REMOTE_PROVIDER_NAME, self.node_name),
                    reason,
                },
                other => other,
            })?;
        Ok(self.stamp(handle))
    }

    async fn image_identity(&self, handle: &SandboxHandle) -> Result<String, AgentError> {
        Err(self.unsupported(
            &handle.sandbox_name,
            "Runtime updates (immutable image identity)",
        ))
    }

    async fn check_agent_runtime(
        &self,
        handle: &SandboxHandle,
    ) -> Result<RuntimeCompatibility, AgentError> {
        Err(self.unsupported(&handle.sandbox_name, "The retained agent runtime"))
    }

    async fn recover_agent_harness(
        &self,
        handle: &SandboxHandle,
        _epoch: u64,
    ) -> Result<(), AgentError> {
        Err(self.unsupported(&handle.sandbox_name, "Agent harness recovery"))
    }

    async fn exec(
        &self,
        handle: &SandboxHandle,
        cmd: Vec<String>,
        env: HashMap<String, String>,
        on_output: Option<OnEventCallback>,
    ) -> Result<SandboxExecResult, AgentError> {
        match on_output {
            Some(cb) => {
                self.run_exec_streamed(handle, cmd, env, None, false, Some(stdout_only(cb)))
                    .await
            }
            None => self.run_exec(handle, cmd, env, None, false).await,
        }
    }

    async fn exec_as_root(
        &self,
        handle: &SandboxHandle,
        cmd: Vec<String>,
        env: HashMap<String, String>,
        on_output: Option<OnEventCallback>,
    ) -> Result<SandboxExecResult, AgentError> {
        match on_output {
            Some(cb) => {
                self.run_exec_streamed(handle, cmd, env, None, true, Some(stdout_only(cb)))
                    .await
            }
            None => self.run_exec(handle, cmd, env, None, true).await,
        }
    }

    async fn exec_as_user(
        &self,
        handle: &SandboxHandle,
        user: &str,
        cmd: Vec<String>,
        env: HashMap<String, String>,
        on_output: Option<OnEventCallback>,
    ) -> Result<SandboxExecResult, AgentError> {
        let user = Some(user.to_string());
        match on_output {
            Some(cb) => {
                self.run_exec_streamed(handle, cmd, env, user, false, Some(stdout_only(cb)))
                    .await
            }
            None => self.run_exec(handle, cmd, env, user, false).await,
        }
    }

    async fn exec_streamed(
        &self,
        handle: &SandboxHandle,
        cmd: Vec<String>,
        env: HashMap<String, String>,
        on_event: Option<OnStreamEventCallback>,
    ) -> Result<SandboxExecResult, AgentError> {
        self.run_exec_streamed(handle, cmd, env, None, false, on_event)
            .await
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
                SMALL_RESPONSE_CAP,
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
        self.call_ok(
            "/agent/sandboxes/write-file",
            &body,
            SHORT_TIMEOUT,
            handle,
            "write_file",
        )
        .await
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
                READ_FILE_RESPONSE_CAP,
            )
            .await?;
        base64::engine::general_purpose::STANDARD
            .decode(r.contents_b64)
            .map_err(|e| {
                self.failed(
                    Some(handle),
                    "read_file",
                    format!("invalid base64 for '{}': {}", path, e),
                )
            })
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
            .map_err(|e| {
                self.failed(
                    Some(handle),
                    "write_directory",
                    format!("archive task for {} failed: {}", local_dir.display(), e),
                )
            })?
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
        self.call_ok(
            "/agent/sandboxes/write-directory",
            &body,
            LIFECYCLE_TIMEOUT,
            handle,
            "write_directory",
        )
        .await
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
        self.call_ok(
            "/agent/sandboxes/kill-processes",
            &body,
            SHORT_TIMEOUT,
            handle,
            "kill_processes",
        )
        .await
    }

    async fn destroy(&self, handle: &SandboxHandle, purge_volumes: bool) -> Result<(), AgentError> {
        let body = RemoteDestroyRequest {
            handle: Self::local_handle(handle),
            purge_volumes,
        };
        self.call_ok(
            "/agent/sandboxes/destroy",
            &body,
            LIFECYCLE_TIMEOUT,
            handle,
            "destroy",
        )
        .await
    }

    async fn stop(&self, handle: &SandboxHandle) -> Result<(), AgentError> {
        let body = RemoteHandleRequest {
            handle: Self::local_handle(handle),
        };
        self.call_ok(
            "/agent/sandboxes/stop",
            &body,
            LIFECYCLE_TIMEOUT,
            handle,
            "stop",
        )
        .await
    }

    async fn start(&self, handle: &SandboxHandle) -> Result<(), AgentError> {
        let body = RemoteHandleRequest {
            handle: Self::local_handle(handle),
        };
        self.call_ok(
            "/agent/sandboxes/start",
            &body,
            LIFECYCLE_TIMEOUT,
            handle,
            "start",
        )
        .await
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
                SMALL_RESPONSE_CAP,
            )
            .await?;
        Ok(handle.map(|h| self.stamp(h)))
    }

    fn supports_backend(&self, backend: SandboxBackend) -> bool {
        backend == SandboxBackend::Docker
    }

    async fn attach_pty(&self, handle: &SandboxHandle) -> Result<PtyAttachment, AgentError> {
        Err(self.unsupported(&handle.sandbox_name, "The interactive terminal"))
    }

    async fn take_snapshot(
        &self,
        handle: &SandboxHandle,
        _label: Option<String>,
        _max_size_bytes: u64,
    ) -> Result<SnapshotArtifact, AgentError> {
        Err(self.unsupported(&handle.sandbox_name, "Snapshots"))
    }

    async fn create_from_snapshot(
        &self,
        _artifact: &SnapshotArtifact,
        config: SandboxCreateConfig,
    ) -> Result<SandboxHandle, AgentError> {
        let label = config
            .container_name_override
            .clone()
            .unwrap_or_else(|| config.run_id.to_string());
        Err(self.unsupported(&label, "Restoring a snapshot"))
    }

    async fn configure_application_network(
        &self,
        handle: &SandboxHandle,
        _network_name: &str,
        _service_containers: &[String],
    ) -> Result<(), AgentError> {
        Err(self.unsupported(&handle.sandbox_name, "Application service networking"))
    }

    async fn git_relay_base_url(
        &self,
        handle: &SandboxHandle,
        _control_plane_url: &str,
    ) -> Result<String, AgentError> {
        Err(self.unsupported(&handle.sandbox_name, "The git relay"))
    }

    // The relays below reach the control plane from inside the sandbox,
    // which is not verified from worker nodes yet (ADR-048 §6). Refuse
    // instead of handing out a URL the worker may not be able to reach.
    async fn model_relay_base_url(
        &self,
        handle: &SandboxHandle,
        _control_plane_url: &str,
    ) -> Result<String, AgentError> {
        Err(self.unsupported(&handle.sandbox_name, "The model relay"))
    }

    async fn harness_mcp_url(
        &self,
        handle: &SandboxHandle,
        _control_plane_url: &str,
        _registered_url: &str,
    ) -> Result<String, AgentError> {
        Err(self.unsupported(&handle.sandbox_name, "The harness MCP relay"))
    }

    async fn connect_agent_runtime(
        &self,
        handle: &SandboxHandle,
    ) -> Result<PtyAttachment, AgentError> {
        Err(self.unsupported(&handle.sandbox_name, "The retained agent runtime"))
    }

    async fn resize_disk(
        &self,
        handle: &SandboxHandle,
        _new_size_mb: u64,
    ) -> Result<(), AgentError> {
        Err(self.unsupported(&handle.sandbox_name, "Disk resize"))
    }

    fn name(&self) -> &str {
        REMOTE_PROVIDER_NAME
    }

    async fn is_available(&self) -> bool {
        self.status().await.map(|s| s.available).unwrap_or(false)
    }

    async fn image_status(&self) -> Result<(bool, String), AgentError> {
        let r = self.status().await?;
        Ok((r.image_ready, sanitize_worker_message(&r.image)))
    }

    async fn rebuild_image(&self) -> Result<String, AgentError> {
        Err(self.unsupported("", "Rebuilding the sandbox image"))
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

    #[test]
    fn labels_cannot_name_the_egress_proxy_sidecar() {
        // `temps-sandbox-egress-proxy-v2-temps-sandbox-x` is the sidecar.
        assert!(!is_valid_sandbox_label("egress-proxy-v2-temps-sandbox-x"));
        assert!(!is_valid_sandbox_label("egress-proxy"));
        assert!(!is_sandbox_handle(&handle(
            "temps-sandbox-egress-proxy-v2-temps-sandbox-abc"
        )));
        // Only the prefix is reserved.
        assert!(is_valid_sandbox_label("my-egress-proxy"));
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
    fn status_request_is_an_empty_object() {
        assert_eq!(
            serde_json::to_string(&RemoteStatusRequest {}).unwrap(),
            "{}"
        );
    }

    fn archive_entries(bytes: &[u8]) -> Vec<(String, tar::EntryType, Vec<u8>)> {
        use std::io::Read;
        let mut archive = tar::Archive::new(bytes);
        archive
            .entries()
            .unwrap()
            .map(|e| {
                let mut e = e.unwrap();
                let name = e.path().unwrap().to_string_lossy().into_owned();
                let kind = e.header().entry_type();
                let mut data = Vec::new();
                e.read_to_end(&mut data).unwrap();
                (name, kind, data)
            })
            .collect()
    }

    #[test]
    fn directory_archive_preserves_relative_paths() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub/a.txt"), b"hi").unwrap();
        let bytes = tar_directory(dir.path()).unwrap();
        let entries = archive_entries(&bytes);
        assert!(
            entries
                .iter()
                .any(|(n, _, d)| n == "sub/a.txt" && d.as_slice() == b"hi"),
            "{entries:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn directory_archive_resolves_inner_links_and_drops_escaping_ones() {
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret"), b"host secret").unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("real")).unwrap();
        std::fs::write(dir.path().join("real/skill.md"), b"skill").unwrap();
        // Inside the tree: kept, as a plain file with the target's bytes.
        std::os::unix::fs::symlink("real/skill.md", dir.path().join("alias.md")).unwrap();
        std::os::unix::fs::symlink("real", dir.path().join("linked-dir")).unwrap();
        // Escaping the tree: dropped.
        std::os::unix::fs::symlink(outside.path().join("secret"), dir.path().join("leak")).unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("leak-dir")).unwrap();
        std::os::unix::fs::symlink("/", dir.path().join("root")).unwrap();

        let entries = archive_entries(&tar_directory(dir.path()).unwrap());
        let names: Vec<&str> = entries.iter().map(|(n, _, _)| n.as_str()).collect();
        assert!(
            entries
                .iter()
                .all(|(_, kind, _)| *kind == tar::EntryType::Regular),
            "only regular files cross the wire: {entries:?}"
        );
        assert!(names.contains(&"real/skill.md"), "{names:?}");
        assert!(names.contains(&"alias.md"), "{names:?}");
        assert!(names.contains(&"linked-dir/skill.md"), "{names:?}");
        assert!(
            !names
                .iter()
                .any(|n| n.starts_with("leak") || n.starts_with("root")),
            "{names:?}"
        );
        assert!(entries
            .iter()
            .all(|(_, _, d)| d.as_slice() != b"host secret"));
    }

    #[test]
    fn worker_messages_lose_control_characters_and_are_capped() {
        assert_eq!(
            sanitize_worker_message("\u{1b}[31mred\u{1b}[0m\r\nnext"),
            " [31mred [0m  next"
        );
        let long = "x".repeat(MAX_WORKER_MESSAGE_CHARS + 10);
        let out = sanitize_worker_message(&long);
        assert_eq!(out.chars().count(), MAX_WORKER_MESSAGE_CHARS + 1);
        assert!(out.ends_with('…'));
        assert_eq!(sanitize_worker_message("short"), "short");
    }

    type Seen = std::sync::Arc<tokio::sync::Mutex<Option<serde_json::Value>>>;
    type Reply = std::sync::Arc<(u16, Option<serde_json::Value>)>;

    async fn serve(app: axum::Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}")
    }

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
        (serve(app).await, seen)
    }

    /// A fake worker answering with raw bytes. `chunked` streams the body
    /// without a Content-Length, so only the read loop can enforce a cap.
    async fn fake_agent_raw(status: u16, body: Vec<u8>, chunked: bool) -> String {
        use axum::{body::Body, http::StatusCode, response::Response};
        let body = std::sync::Arc::new(body);
        let app = axum::Router::new().fallback(move || {
            let body = body.clone();
            async move {
                let payload = if chunked {
                    let chunks: Vec<Result<Vec<u8>, std::io::Error>> =
                        body.chunks(1024).map(|c| Ok(c.to_vec())).collect();
                    Body::from_stream(futures::stream::iter(chunks))
                } else {
                    Body::from(body.as_ref().clone())
                };
                Response::builder()
                    .status(StatusCode::from_u16(status).unwrap())
                    .header("content-type", "application/json")
                    .body(payload)
                    .unwrap()
            }
        });
        serve(app).await
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

    fn create_config() -> SandboxCreateConfig {
        SandboxCreateConfig {
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
        }
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
            matches!(&err, AgentError::SandboxExecFailed { reason, .. }
                if reason.contains("out of memory") && reason.contains("worker-2") && reason.contains("read_file")),
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
        assert!(
            matches!(&err, AgentError::Validation { message }
                if message.contains("detail") && message.contains("worker-2") && message.contains("stop")),
            "{err:?}"
        );

        for status in [401, 403] {
            let (refused, _) = fake_agent(status, body()).await;
            let err = provider_at(&refused)
                .stop(&remote_handle())
                .await
                .unwrap_err();
            assert!(
                matches!(&err, AgentError::SandboxProviderUnavailable { reason, .. } if reason.contains("re-join")),
                "{status}: {err:?}"
            );
        }

        let (conflict, _) = fake_agent(409, body()).await;
        let err = provider_at(&conflict)
            .create(create_config())
            .await
            .unwrap_err();
        // A conflict is its own error: the caller must not tear anything
        // down for it (the conflicting sandbox is not the caller's).
        assert!(
            matches!(&err, AgentError::SandboxConflictOnNode { node_name, reason, .. }
                if node_name == "worker-2" && reason.contains("detail")),
            "{err:?}"
        );

        let (unsupported, _) = fake_agent(422, body()).await;
        let err = provider_at(&unsupported)
            .stop(&remote_handle())
            .await
            .unwrap_err();
        assert!(
            matches!(&err, AgentError::SandboxUnsupportedOnNode { node_name, feature, .. }
                if node_name == "worker-2" && feature == "stop"),
            "{err:?}"
        );

        let (bare_500, _) = fake_agent(500, None).await;
        let err = provider_at(&bare_500)
            .stop(&remote_handle())
            .await
            .unwrap_err();
        assert!(
            matches!(&err, AgentError::SandboxExecFailed { reason, .. } if reason.contains("HTTP 500")),
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
    async fn transport_errors_never_reveal_the_agent_url() {
        for url in [
            "http://127.0.0.1:9",
            "http://internal-node-address.invalid:7443",
        ] {
            let err = provider_at(url)
                .is_alive(&remote_handle())
                .await
                .unwrap_err();
            let text = err.to_string();
            assert!(!text.contains("127.0.0.1"), "{text}");
            assert!(!text.contains("internal-node-address"), "{text}");
            assert!(!text.contains(":9"), "{text}");
            assert!(!text.contains("7443"), "{text}");
            assert!(!text.contains("http"), "{text}");
            assert!(text.contains("worker-2"), "{text}");
            assert!(text.contains("is_alive"), "{text}");
        }
    }

    #[tokio::test]
    async fn worker_error_text_is_sanitized() {
        let hostile = format!("\u{1b}]0;pwned\u{7}\u{1b}[2J{}", "y".repeat(2000));
        let (url, _) = fake_agent(500, Some(serde_json::json!({ "error": hostile }))).await;
        let err = provider_at(&url).stop(&remote_handle()).await.unwrap_err();
        let text = err.to_string();
        assert!(!text.chars().any(|c| c.is_control()), "{text:?}");
        assert!(text.contains('…'), "{text}");
        assert!(
            text.chars().count() < MAX_WORKER_MESSAGE_CHARS + 300,
            "{}",
            text.len()
        );
    }

    #[tokio::test]
    async fn oversized_error_bodies_are_not_buffered() {
        for chunked in [false, true] {
            let body = serde_json::to_vec(&serde_json::json!({
                "error": "z".repeat(ERROR_BODY_CAP * 4)
            }))
            .unwrap();
            let url = fake_agent_raw(500, body, chunked).await;
            let err = provider_at(&url).stop(&remote_handle()).await.unwrap_err();
            let text = err.to_string();
            assert!(text.contains("discarded"), "chunked={chunked}: {text}");
            assert!(!text.contains("zzzz"), "chunked={chunked}: {text}");
        }
    }

    #[tokio::test]
    async fn oversized_responses_fail_naming_the_node_and_operation() {
        for chunked in [false, true] {
            let body = serde_json::to_vec(&serde_json::json!({
                "alive": true,
                "padding": "p".repeat(8 * 1024),
            }))
            .unwrap();
            let url = fake_agent_raw(200, body, chunked).await;
            let provider = provider_at(&url);
            let handle = remote_handle();
            let err = provider
                .call::<_, RemoteAliveResponse>(
                    "/agent/sandboxes/alive",
                    &RemoteHandleRequest {
                        handle: handle.clone(),
                    },
                    SHORT_TIMEOUT,
                    Some(&handle),
                    "is_alive",
                    1024,
                )
                .await
                .unwrap_err();
            match err {
                AgentError::SandboxExecFailed { reason, .. } => {
                    assert!(reason.contains("worker-2"), "{reason}");
                    assert!(reason.contains("is_alive"), "{reason}");
                    assert!(reason.contains("1024 byte limit"), "{reason}");
                }
                other => panic!("chunked={chunked}: {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn responses_within_their_cap_still_decode() {
        let body = serde_json::to_vec(&serde_json::json!({ "alive": true })).unwrap();
        let url = fake_agent_raw(200, body, true).await;
        assert!(provider_at(&url).is_alive(&remote_handle()).await.unwrap());
    }

    fn assert_unsupported(err: AgentError, feature: &str) {
        match &err {
            AgentError::SandboxUnsupportedOnNode {
                node_name,
                feature: f,
                ..
            } => {
                assert_eq!(node_name, "worker-2");
                assert!(f.contains(feature), "{f}");
                let text = err.to_string();
                assert!(text.contains("worker-2"), "{text}");
                assert!(!text.contains("provider 'remote'"), "{text}");
            }
            other => panic!("expected unsupported-on-node for {feature}, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn unsupported_features_are_named_errors_without_calling_the_node() {
        let (url, seen) = fake_agent(200, Some(serde_json::json!({"ok": true}))).await;
        let provider = provider_at(&url);
        let h = remote_handle();

        assert_unsupported(
            provider.attach_pty(&h).await.err().unwrap(),
            "interactive terminal",
        );
        assert_unsupported(
            provider.check_agent_runtime(&h).await.unwrap_err(),
            "retained agent runtime",
        );
        assert_unsupported(
            provider.recover_agent_harness(&h, 1).await.unwrap_err(),
            "harness recovery",
        );
        assert_unsupported(
            provider.image_identity(&h).await.unwrap_err(),
            "image identity",
        );
        assert_unsupported(
            provider.resize_disk(&h, 1024).await.unwrap_err(),
            "Disk resize",
        );
        assert_unsupported(provider.rebuild_image().await.unwrap_err(), "sandbox image");

        let mut with_volume = create_config();
        with_volume.workspace_volume = Some("vol".into());
        assert_unsupported(
            provider.create(with_volume).await.unwrap_err(),
            "Workspace volumes",
        );
        let mut firecracker = create_config();
        firecracker.backend = Some(SandboxBackend::Firecracker);
        assert_unsupported(
            provider.create(firecracker).await.unwrap_err(),
            "firecracker",
        );

        assert!(seen.lock().await.is_none(), "no request reached the node");
    }

    // ── Streamed exec ──────────────────────────────────────────────────

    /// One step of a fake worker's exec stream.
    enum Step {
        Frame(RemoteExecFrame),
        Raw(Vec<u8>),
        Sleep(Duration),
        Wait(std::sync::Arc<tokio::sync::Notify>),
        Hang,
    }

    type Paths = std::sync::Arc<std::sync::Mutex<Vec<String>>>;

    /// A fake worker whose `exec-stream` route plays `steps`, and whose
    /// single-response `exec` route answers `single`. Records which routes
    /// were called.
    async fn fake_stream_agent(steps: Vec<Step>) -> (String, Paths) {
        use axum::{body::Body, response::Response, routing::post, Json};
        let paths: Paths = Default::default();
        let steps = std::sync::Arc::new(std::sync::Mutex::new(Some(steps)));
        let (p1, p2) = (paths.clone(), paths.clone());
        let app = axum::Router::new()
            .route(
                "/agent/sandboxes/exec-stream",
                post(move |Json(_req): Json<RemoteExecRequest>| {
                    let steps = steps.lock().unwrap().take().unwrap_or_default();
                    p1.lock().unwrap().push("exec-stream".into());
                    async move {
                        let body =
                            futures::stream::unfold(steps.into_iter(), |mut it| async move {
                                loop {
                                    match it.next()? {
                                        Step::Frame(f) => {
                                            let mut b = serde_json::to_vec(&f).unwrap();
                                            b.push(b'\n');
                                            return Some((Ok::<_, std::io::Error>(b), it));
                                        }
                                        Step::Raw(b) => return Some((Ok(b), it)),
                                        Step::Sleep(d) => tokio::time::sleep(d).await,
                                        Step::Wait(n) => n.notified().await,
                                        Step::Hang => std::future::pending::<()>().await,
                                    }
                                }
                            });
                        Response::builder()
                            .header("content-type", "application/x-ndjson")
                            .body(Body::from_stream(body))
                            .unwrap()
                    }
                }),
            )
            .route(
                "/agent/sandboxes/exec",
                post(move |Json(_req): Json<RemoteExecRequest>| {
                    p2.lock().unwrap().push("exec".into());
                    async move {
                        Json(RemoteExecResponse {
                            exit_code: 0,
                            stdout: "single\n".into(),
                            stderr: String::new(),
                        })
                    }
                }),
            );
        (serve(app).await, paths)
    }

    fn out(line: &str) -> Step {
        Step::Frame(RemoteExecFrame::Stdout {
            line: line.into(),
            more: false,
        })
    }

    fn err_line(line: &str) -> Step {
        Step::Frame(RemoteExecFrame::Stderr {
            line: line.into(),
            more: false,
        })
    }

    fn exit(exit_code: i32) -> Step {
        Step::Frame(RemoteExecFrame::Exit { exit_code })
    }

    type Lines = tokio::sync::mpsc::UnboundedReceiver<(ExecStream, String)>;

    fn recording_callback() -> (OnStreamEventCallback, Lines) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let cb: OnStreamEventCallback = std::sync::Arc::new(move |stream, line| {
            let tx = tx.clone();
            Box::pin(async move {
                let _ = tx.send((stream, line));
            })
        });
        (cb, rx)
    }

    #[tokio::test]
    async fn exec_streamed_delivers_lines_while_the_command_runs() {
        let release = std::sync::Arc::new(tokio::sync::Notify::new());
        let (url, paths) = fake_stream_agent(vec![
            out("server listening"),
            // The command keeps running until the test has seen the first
            // line: output must arrive live, not after exit.
            Step::Wait(release.clone()),
            err_line("warning: slow"),
            Step::Frame(RemoteExecFrame::Heartbeat),
            exit(3),
        ])
        .await;
        let (cb, mut lines) = recording_callback();
        let provider = provider_at(&url);
        let run = tokio::spawn(async move {
            provider
                .exec_streamed(
                    &remote_handle(),
                    vec!["npm".into(), "run".into(), "dev".into()],
                    HashMap::new(),
                    Some(cb),
                )
                .await
        });
        let first = tokio::time::timeout(Duration::from_secs(5), lines.recv())
            .await
            .expect("the first line arrives before the command exits")
            .unwrap();
        assert_eq!(first, (ExecStream::Stdout, "server listening".to_string()));
        assert!(!run.is_finished());
        release.notify_one();
        let result = run.await.unwrap().unwrap();
        assert_eq!(result.exit_code, 3);
        assert_eq!(result.stdout, "server listening\n");
        assert_eq!(result.stderr, "warning: slow\n");
        assert_eq!(
            lines.recv().await.unwrap(),
            (ExecStream::Stderr, "warning: slow".to_string())
        );
        assert_eq!(*paths.lock().unwrap(), vec!["exec-stream".to_string()]);
    }

    #[tokio::test]
    async fn quiet_commands_are_kept_alive_by_heartbeats() {
        let mut steps = Vec::new();
        for _ in 0..8 {
            steps.push(Step::Sleep(Duration::from_millis(100)));
            steps.push(Step::Frame(RemoteExecFrame::Heartbeat));
        }
        steps.push(exit(0));
        let (url, _) = fake_stream_agent(steps).await;
        // Total runtime (~800 ms) is well past the idle timeout; only
        // silence counts.
        let result = provider_at(&url)
            .with_exec_stream_idle_timeout(Duration::from_millis(400))
            .exec_streamed(&remote_handle(), vec!["sleep".into()], HashMap::new(), None)
            .await
            .unwrap();
        assert_eq!(result.exit_code, 0);
    }

    #[tokio::test]
    async fn a_silent_exec_stream_means_the_node_is_unavailable() {
        let (url, _) = fake_stream_agent(vec![out("started"), Step::Hang]).await;
        let err = provider_at(&url)
            .with_exec_stream_idle_timeout(Duration::from_millis(200))
            .exec_streamed(&remote_handle(), vec!["x".into()], HashMap::new(), None)
            .await
            .err()
            .expect("the exec fails");
        match &err {
            AgentError::SandboxProviderUnavailable { reason, .. } => {
                assert!(reason.contains("worker-2"), "{reason}");
                assert!(reason.contains("sent nothing"), "{reason}");
                assert!(reason.contains("temps-sandbox-abc"), "{reason}");
            }
            other => panic!("expected unavailable, got {other:?}"),
        }
        let text = err.to_string();
        assert!(
            !text.contains("127.0.0.1") && !text.contains("http"),
            "{text}"
        );
    }

    #[tokio::test]
    async fn an_exec_stream_cut_short_is_not_a_success() {
        let (url, _) = fake_stream_agent(vec![out("partial")]).await;
        let err = provider_at(&url)
            .exec_streamed(&remote_handle(), vec!["x".into()], HashMap::new(), None)
            .await
            .err()
            .expect("the exec fails");
        assert!(
            matches!(&err, AgentError::SandboxProviderUnavailable { reason, .. }
                if reason.contains("before the command finished")),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn exec_stream_error_frames_map_like_error_statuses() {
        let error = |status: u16, error: &str| {
            Step::Frame(RemoteExecFrame::Error {
                status,
                error: error.into(),
            })
        };
        let (gone, _) = fake_stream_agent(vec![error(404, "container vanished")]).await;
        let err = provider_at(&gone)
            .exec_streamed(&remote_handle(), vec!["x".into()], HashMap::new(), None)
            .await
            .err()
            .expect("the exec fails");
        assert!(matches!(err, AgentError::SandboxNotFound { .. }), "{err:?}");

        let hostile = format!("\u{1b}[2Jdocker exec failed {}", "y".repeat(2000));
        let (failed, _) = fake_stream_agent(vec![out("a"), error(500, &hostile)]).await;
        let err = provider_at(&failed)
            .exec_streamed(&remote_handle(), vec!["x".into()], HashMap::new(), None)
            .await
            .err()
            .expect("the exec fails");
        let text = err.to_string();
        assert!(
            matches!(&err, AgentError::SandboxExecFailed { reason, .. }
                if reason.contains("docker exec failed") && reason.contains("worker-2")),
            "{err:?}"
        );
        assert!(!text.chars().any(|c| c.is_control()), "{text:?}");
        assert!(text.contains('…'), "{text}");

        // A "success" status cannot describe a failure.
        let (odd, _) = fake_stream_agent(vec![error(200, "odd")]).await;
        let err = provider_at(&odd)
            .exec_streamed(&remote_handle(), vec!["x".into()], HashMap::new(), None)
            .await
            .err()
            .expect("the exec fails");
        assert!(
            matches!(&err, AgentError::SandboxExecFailed { reason, .. } if reason.contains("odd")),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn exec_stream_refusals_before_streaming_keep_their_status() {
        let (url, _) = fake_agent(
            404,
            Some(serde_json::json!({"error": "sandbox container 'temps-sandbox-abc' does not exist"})),
        )
        .await;
        let err = provider_at(&url)
            .exec_streamed(&remote_handle(), vec!["x".into()], HashMap::new(), None)
            .await
            .err()
            .expect("the exec fails");
        assert!(matches!(err, AgentError::SandboxNotFound { .. }), "{err:?}");
    }

    #[tokio::test]
    async fn oversized_or_invalid_exec_frames_are_refused() {
        // No newline ever: only the frame cap stops the buffering.
        let (url, _) = fake_stream_agent(vec![Step::Raw(vec![b'a'; EXEC_FRAME_CAP + 1])]).await;
        let err = provider_at(&url)
            .exec_streamed(&remote_handle(), vec!["x".into()], HashMap::new(), None)
            .await
            .err()
            .expect("the exec fails");
        assert!(
            matches!(&err, AgentError::SandboxExecFailed { reason, .. } if reason.contains("byte limit")),
            "{err:?}"
        );

        let (url, _) = fake_stream_agent(vec![Step::Raw(b"{\"type\":\"bogus\"}\n".to_vec())]).await;
        let err = provider_at(&url)
            .exec_streamed(&remote_handle(), vec!["x".into()], HashMap::new(), None)
            .await
            .err()
            .expect("the exec fails");
        assert!(
            matches!(&err, AgentError::SandboxExecFailed { reason, .. } if reason.contains("invalid exec frame")),
            "{err:?}"
        );
    }

    /// A whole oversized frame, newline included, arriving in one chunk must
    /// be refused before it is parsed or reaches the callback.
    #[tokio::test]
    async fn a_complete_oversized_exec_frame_is_refused_before_it_is_used() {
        let line = "x".repeat(EXEC_FRAME_CAP);
        let mut frame = serde_json::to_vec(&RemoteExecFrame::Stdout { line, more: false }).unwrap();
        frame.push(b'\n');
        let (url, _) = fake_stream_agent(vec![Step::Raw(frame), exit(0)]).await;
        let (cb, mut lines) = recording_callback();
        let err = provider_at(&url)
            .exec_streamed(&remote_handle(), vec!["x".into()], HashMap::new(), Some(cb))
            .await
            .err()
            .expect("the exec fails");
        assert!(
            matches!(&err, AgentError::SandboxExecFailed { reason, .. } if reason.contains("byte limit")),
            "{err:?}"
        );
        assert!(lines.try_recv().is_err(), "nothing reached the callback");
    }

    #[tokio::test]
    async fn long_lines_are_rebuilt_from_continuation_frames() {
        let long = format!(
            "{}é{}",
            "a".repeat(WORKER_EXEC_LINE_LIMIT - 1),
            "b".repeat(70_000)
        );
        let mut steps: Vec<Step> = RemoteExecFrame::output_frames(ExecStream::Stdout, long.clone())
            .into_iter()
            .map(Step::Frame)
            .collect();
        steps.push(err_line("warn"));
        // A line cut off by the end of the command still arrives.
        steps.push(Step::Frame(RemoteExecFrame::Stdout {
            line: "partial".into(),
            more: true,
        }));
        steps.push(exit(0));
        let (url, _) = fake_stream_agent(steps).await;
        let (cb, mut lines) = recording_callback();
        let result = provider_at(&url)
            .exec_streamed(&remote_handle(), vec!["x".into()], HashMap::new(), Some(cb))
            .await
            .unwrap();
        assert_eq!(lines.recv().await, Some((ExecStream::Stdout, long.clone())));
        assert_eq!(
            lines.recv().await,
            Some((ExecStream::Stderr, "warn".into()))
        );
        assert_eq!(
            lines.recv().await,
            Some((ExecStream::Stdout, "partial".into()))
        );
        assert_eq!(result.stdout, format!("{long}\npartial\n"));
    }

    #[test]
    fn a_rebuilt_line_is_capped_at_the_stream_limit() {
        let mut assembler = LineAssembler::new(10);
        assert_eq!(assembler.push("aaaaaaaa", true), None);
        // "é" straddles the limit and is dropped whole.
        assert_eq!(assembler.push("bé", true), None);
        let line = assembler.push("ccc", false).unwrap();
        assert_eq!(
            line,
            "aaaaaaaab [5 more bytes of this line truncated on the control plane]"
        );
        assert_eq!(assembler.flush(), None);
        assert_eq!(assembler.push("next", false).as_deref(), Some("next"));
    }

    #[tokio::test]
    async fn exec_streams_only_with_a_callback_and_then_only_stdout() {
        let (url, paths) = fake_stream_agent(vec![out("o"), err_line("e"), exit(0)]).await;
        let provider = provider_at(&url);
        // No callback: the single-response route, as before.
        let result = provider
            .exec(&remote_handle(), vec!["x".into()], HashMap::new(), None)
            .await
            .unwrap();
        assert_eq!(result.stdout, "single\n");

        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let seen_cb = seen.clone();
        let cb: OnEventCallback = std::sync::Arc::new(move |line| {
            seen_cb.lock().unwrap().push(line);
            Box::pin(async {})
        });
        let result = provider
            .exec(&remote_handle(), vec!["x".into()], HashMap::new(), Some(cb))
            .await
            .unwrap();
        assert_eq!(*seen.lock().unwrap(), vec!["o".to_string()]);
        assert_eq!(result.stderr, "e\n");
        assert_eq!(
            *paths.lock().unwrap(),
            vec!["exec".to_string(), "exec-stream".to_string()]
        );
    }

    #[test]
    fn long_output_lines_are_split_on_the_worker_not_cut() {
        let line = format!("{}é tail", "x".repeat(WORKER_EXEC_LINE_LIMIT - 1));
        let frames = RemoteExecFrame::output_frames(ExecStream::Stdout, line.clone());
        // The two-byte character straddling the limit moves to the next piece.
        assert_eq!(
            frames,
            vec![
                RemoteExecFrame::Stdout {
                    line: "x".repeat(WORKER_EXEC_LINE_LIMIT - 1),
                    more: true,
                },
                RemoteExecFrame::Stdout {
                    line: "é tail".into(),
                    more: false,
                },
            ]
        );
        assert_eq!(
            RemoteExecFrame::output_frames(ExecStream::Stderr, "short".into()),
            vec![RemoteExecFrame::Stderr {
                line: "short".into(),
                more: false,
            }]
        );
        // `more` is only on the wire when set, so a plain line keeps its shape.
        assert_eq!(
            serde_json::to_string(&frames[1]).unwrap(),
            r#"{"type":"stdout","line":"é tail"}"#
        );
        assert_eq!(
            serde_json::to_string(&frames[0]).unwrap(),
            format!(
                r#"{{"type":"stdout","line":"{}","more":true}}"#,
                "x".repeat(WORKER_EXEC_LINE_LIMIT - 1)
            )
        );
        let frame = serde_json::to_string(&RemoteExecFrame::Heartbeat).unwrap();
        assert_eq!(frame, r#"{"type":"heartbeat"}"#);
    }

    #[test]
    fn exec_stream_output_keeps_a_bounded_tail() {
        let mut tail = OutputTail::new(10);
        tail.push_line("short");
        assert_eq!(OutputTail::new(10).finish(), "");
        for i in 0..100 {
            tail.push_line(&format!("line{i}"));
            assert!(tail.text.len() <= 10 + 10 / 4 + 7, "{}", tail.text.len());
        }
        let out = tail.finish();
        assert!(out.ends_with("line99\n"), "{out}");
        assert!(out.starts_with("["), "{out}");
        assert!(out.contains("truncated on the control plane"), "{out}");
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
        provider.create(create_config()).await.unwrap();
        let body = seen.lock().await.clone().unwrap();
        assert_eq!(body["network_mode"], "none");
        assert_eq!(body["image"], "img:1");
        assert_eq!(body["memory_limit_mb"], 512);
    }

    #[tokio::test]
    async fn create_treats_an_empty_image_as_unset() {
        let created = serde_json::to_value(remote_handle()).unwrap();
        for (requested, expected) in [(Some(""), "img:1"), (Some("custom:2"), "custom:2")] {
            let (url, seen) = fake_agent(200, Some(created.clone())).await;
            let provider = provider_at(&url).with_defaults(RemoteSandboxDefaults {
                image: Some("img:1".into()),
                ..Default::default()
            });
            let mut config = create_config();
            config.image = requested.map(str::to_string);
            provider.create(config).await.unwrap();
            let body = seen.lock().await.clone().unwrap();
            assert_eq!(body["image"], expected, "requested {requested:?}");
        }
    }
}
