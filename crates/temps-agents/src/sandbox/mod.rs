pub mod docker;
pub mod firecracker;
pub mod git_credential_bundle;
pub mod local;
pub mod pty_agent_bundle;
pub mod routing;
pub mod user;

pub use user::{
    SANDBOX_CHOWN, SANDBOX_GID, SANDBOX_GROUP, SANDBOX_HOME, SANDBOX_UID, SANDBOX_USER,
    SANDBOX_WORK_DIR,
};

use async_trait::async_trait;
use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use crate::ai_cli::OnEventCallback;
use crate::error::AgentError;

/// Stream an exec line came from. Mirrors the `{stream, data}` shape that
/// the `@vercel/sandbox` `Command.logs()` async iterator yields, so we can
/// surface the same structure to callers of the standalone sandbox API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecStream {
    Stdout,
    Stderr,
}

/// Callback invoked for each line of sandbox exec output, tagged by which
/// stream the line came from. A superset of [`OnEventCallback`] — providers
/// that can split streams should use this variant via `exec_streamed`.
pub type OnStreamEventCallback =
    Arc<dyn Fn(ExecStream, String) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

/// Unix signal to send to processes inside a sandbox. Constrained on purpose
/// — the only two signals the workspace ever needs are SIGTERM (graceful
/// shutdown, give the CLI a chance to flush state) and SIGKILL (hard kill
/// after a grace period). Passing arbitrary integers across the provider
/// boundary would invite untrusted callers to stuff anything from SIGSTOP
/// to SIGUSR1 into the sandbox exec.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KillSignal {
    /// SIGTERM (15) — graceful termination. The process may trap it.
    Term,
    /// SIGKILL (9) — immediate kill. Cannot be trapped.
    Kill,
}

impl KillSignal {
    /// Unix signal number used by `kill(1)` / `pkill -<n>`.
    pub fn as_number(self) -> i32 {
        match self {
            KillSignal::Term => 15,
            KillSignal::Kill => 9,
        }
    }
}

/// Which isolation backend a sandbox runs on (ADR-029). Docker containers
/// and Firecracker microVMs coexist on the same host behind the same
/// `SandboxProvider` seam; `routing::RoutingSandboxProvider` dispatches
/// between them. `Local` is the dev-only fork-exec fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SandboxBackend {
    Docker,
    Firecracker,
    Local,
}

impl std::str::FromStr for SandboxBackend {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "docker" => Ok(Self::Docker),
            "firecracker" => Ok(Self::Firecracker),
            "local" => Ok(Self::Local),
            other => Err(format!("unknown sandbox backend '{}'", other)),
        }
    }
}

impl std::fmt::Display for SandboxBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Docker => "docker",
            Self::Firecracker => "firecracker",
            Self::Local => "local",
        })
    }
}

/// A handle to an active sandbox. Opaque to callers — the internal fields
/// are provider-specific (Docker container ID, Vercel sandbox ID, etc.).
#[derive(Debug, Clone)]
pub struct SandboxHandle {
    /// Provider-specific identifier (container ID, Vercel sandbox ID, etc.)
    pub sandbox_id: String,
    /// Human-readable name for logging (e.g. `temps-sandbox-42`)
    pub sandbox_name: String,
    /// Path to the repository inside the sandbox. See `sandbox::user::SANDBOX_WORK_DIR`.
    pub work_dir: PathBuf,
    /// Which backend owns this sandbox, stamped by the concrete provider
    /// that created or recovered it. Callers read this instead of parsing
    /// the container-name prefix: the routing provider dispatches on it and
    /// the standalone API persists it to `sandboxes.backend` for display.
    pub backend: SandboxBackend,
    /// The image the provider actually resolved to. When the request omitted
    /// an image, this is the backend's default (e.g. `alpine:3.20` for
    /// Firecracker) rather than empty — so the API can show what really
    /// booted instead of a vague "platform default". Empty on recovered
    /// handles (the DB already holds the recorded value).
    pub image: String,
}

/// Configuration for creating a new sandbox.
pub struct SandboxCreateConfig {
    /// The agent run this sandbox belongs to. Used as the fallback
    /// container suffix (`temps-sandbox-<run_id>`) and for the per-run
    /// home volume.
    pub run_id: i32,
    /// When `Some(id)`, the container is named `temps-sandbox-<id>`
    /// instead of `temps-sandbox-<run_id>`. Standalone sandboxes pass
    /// their opaque `public_id` here so the preview URL hostname
    /// (`ws-<id>-<port>.<domain>`) is unguessable. `None` preserves the
    /// historical numeric naming for agent runs and workspace sessions.
    pub container_name_override: Option<String>,
    /// Path to the cloned repository on the host (for bind mount / upload).
    /// When `workspace_volume` is also set, this directory is used to seed
    /// the volume on first use (only if the volume is empty) and is then
    /// ignored — the volume is the source of truth.
    pub host_work_dir: PathBuf,
    /// When `Some`, mount this Docker named volume at the sandbox work dir instead
    /// of bind-mounting `host_work_dir`. The volume is seeded from
    /// `host_work_dir` on first use (detected by checking if it's empty)
    /// and retained on sandbox destroy so a follow-up workspace can mount
    /// the exact same filesystem. This is how "Open in workspace" picks
    /// up where a failed workflow run left off — including `.git` and any
    /// unpushed commits the AI produced.
    ///
    /// Only honored by the Docker provider; `LocalSandboxProvider` ignores
    /// it and falls back to `host_work_dir`.
    pub workspace_volume: Option<String>,
    /// Custom Docker image override (empty = use provider default)
    pub image: Option<String>,
    /// CPU limit in cores (e.g. 2.0)
    pub cpu_limit: Option<f64>,
    /// Memory limit in megabytes
    pub memory_limit_mb: Option<u64>,
    /// Maximum number of processes / threads (PID cgroup limit). When None
    /// the provider default applies.
    pub pids_limit: Option<i64>,
    /// Root disk size in megabytes. Only the Firecracker backend honors it
    /// (the per-VM ext4 is grown to this size); Docker ignores it. `None`
    /// uses the provider default (1 GiB). Values below the image's content
    /// size are clamped up so the image always fits.
    pub disk_size_mb: Option<u64>,
    /// Network access: "full", "restricted", "none"
    pub network_mode: Option<String>,
    /// Environment variables to inject (ANTHROPIC_API_KEY, etc.)
    pub env_vars: HashMap<String, String>,
    /// Maximum time the sandbox should stay alive without activity
    pub idle_timeout: Duration,
    /// Isolation backend for this sandbox. `None` = the host's configured
    /// default. Only meaningful when the registered provider is the
    /// routing provider; single-backend hosts ignore it.
    pub backend: Option<SandboxBackend>,
}

/// A cached rootfs image (Firecracker backend). Digest-keyed build artifact
/// shared by all VMs created from the same image.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct RootfsCacheEntry {
    /// Image digest this rootfs was built from (the cache key).
    pub digest: String,
    /// Actual on-disk size in bytes (sparse-aware).
    pub bytes: u64,
    /// IDs of live sandboxes whose per-VM disk was cloned from this entry.
    /// Empty means the entry is reclaimable — no sandbox needs it.
    pub referenced_by: Vec<String>,
}

/// A per-sandbox rootfs disk (Firecracker backend). One per non-destroyed
/// sandbox — the authoritative storage, independent of the cache.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct RootfsVmEntry {
    pub sandbox_name: String,
    pub bytes: u64,
    pub running: bool,
}

/// Snapshot of a backend's rootfs storage for the management API. Backends
/// without a rootfs concept (Docker, local) return an empty report.
#[derive(Debug, Clone, Default, serde::Serialize, utoipa::ToSchema)]
pub struct RootfsReport {
    pub cache_bytes: u64,
    pub cache: Vec<RootfsCacheEntry>,
    pub vm_bytes: u64,
    pub vms: Vec<RootfsVmEntry>,
}

/// Outcome of a rootfs garbage-collection pass.
#[derive(Debug, Clone, Default, serde::Serialize, utoipa::ToSchema)]
pub struct RootfsGcReport {
    /// Digests of cache entries removed because no sandbox referenced them.
    pub removed_digests: Vec<String>,
    pub freed_bytes: u64,
}

/// Result of executing a command inside a sandbox.
///
/// Historically `stdout` carried the combined stdout+stderr for agent-run
/// callers that never needed them split. The `stderr` field is now populated
/// separately by providers that can distinguish them (Docker); callers that
/// want the combined view can append `stderr` to `stdout` at the call site.
/// Existing agent-run callers ignore `stderr` and continue to read the merged
/// `stdout` unchanged.
pub struct SandboxExecResult {
    pub exit_code: i32,
    pub stdout: String,
    /// Captured stderr. Empty string when the provider can't split streams
    /// (e.g. `LocalSandboxProvider`) or when no stderr was produced.
    pub stderr: String,
}

/// Pluggable sandbox backend. Implementations provide container/VM isolation
/// for agent runs. The executor and autofixer interact only with this trait,
/// never with Docker or any specific backend directly.
///
/// # Boundary contract
///
/// This trait is **the** sandbox boundary for the workspace. Every consumer
/// — `temps-agents` executor, `temps-sandbox` standalone API, autofixer,
/// workspace session manager — holds `Arc<dyn SandboxProvider>` and must
/// never reach past it into Docker, bollard, or any specific backend. Adding
/// a new sandbox backend means adding a new `impl SandboxProvider`, not a
/// new trait.
///
/// The trait lives in `temps-agents` (rather than a neutral crate) because
/// historically agent runs were the only consumer. It stays here for now
/// to avoid churning 10+ downstream imports; if a third family of consumers
/// appears, lift it into a dedicated `temps-sandbox-api` crate.
///
/// **Object-safety** is asserted by the `compile_asserts_object_safety` test
/// in this module. Changes to method signatures must preserve that property
/// (no generic methods, no `Self` return types) — otherwise every consumer's
/// `Arc<dyn SandboxProvider>` stops compiling.
#[async_trait]
pub trait SandboxProvider: Send + Sync {
    /// Create and start a new sandbox for a run.
    async fn create(&self, config: SandboxCreateConfig) -> Result<SandboxHandle, AgentError>;

    /// Execute a command inside an existing sandbox, streaming stdout via callback.
    async fn exec(
        &self,
        handle: &SandboxHandle,
        cmd: Vec<String>,
        env: HashMap<String, String>,
        on_output: Option<OnEventCallback>,
    ) -> Result<SandboxExecResult, AgentError>;

    /// Execute a command as the sandbox container's root user.
    ///
    /// Used for setup-time tasks that must touch files owned by uids
    /// other than the sandbox user (`temps`/uid 1000). The credential
    /// daemon's env file at `/etc/temps/credential-daemon.env` is one
    /// such case: it must be owned by `temps-git`/uid 1001, mode 0600.
    ///
    /// The default implementation falls back to the regular [`Self::exec`]
    /// — useful for providers that don't have a uid concept (e.g. local
    /// fork-and-exec). Docker overrides this to set `user: 0:0` on the
    /// underlying exec config.
    async fn exec_as_root(
        &self,
        handle: &SandboxHandle,
        cmd: Vec<String>,
        env: HashMap<String, String>,
        on_output: Option<OnEventCallback>,
    ) -> Result<SandboxExecResult, AgentError> {
        self.exec(handle, cmd, env, on_output).await
    }

    /// Execute a command as a specific user inside the sandbox.
    ///
    /// `user` is whatever the underlying provider accepts — for Docker
    /// that's a `uid[:gid]` string or a name (e.g. `"1001:1001"` or
    /// `"temps-git:temps-git"`).
    ///
    /// Used to write the credential daemon's env file as the
    /// `temps-git` uid (1001) directly, sidestepping the `CapDrop=ALL`
    /// limitation that prevents root from writing into 0700 dirs owned
    /// by other users (root has CHOWN+FOWNER but not DAC_OVERRIDE in
    /// our sandbox config).
    ///
    /// Default: falls back to [`Self::exec`] (no uid concept).
    async fn exec_as_user(
        &self,
        handle: &SandboxHandle,
        user: &str,
        cmd: Vec<String>,
        env: HashMap<String, String>,
        on_output: Option<OnEventCallback>,
    ) -> Result<SandboxExecResult, AgentError> {
        let _ = user;
        self.exec(handle, cmd, env, on_output).await
    }

    /// Execute a command with a stream-tagged callback — the callback
    /// receives `(ExecStream::Stdout|Stderr, line)` for every line as it
    /// arrives. Providers that can distinguish streams (Docker) override
    /// this; the default falls back to `exec`, which only emits stdout.
    ///
    /// Used by `temps-sandbox` to surface split stdout/stderr and the
    /// `Command.logs()` SSE endpoint to `@vercel/sandbox` consumers.
    async fn exec_streamed(
        &self,
        handle: &SandboxHandle,
        cmd: Vec<String>,
        env: HashMap<String, String>,
        on_event: Option<OnStreamEventCallback>,
    ) -> Result<SandboxExecResult, AgentError> {
        let adapter: Option<OnEventCallback> = on_event.map(|cb| {
            let cb = cb.clone();
            let f: OnEventCallback = Arc::new(move |line: String| {
                let cb = cb.clone();
                let fut: Pin<Box<dyn Future<Output = ()> + Send>> =
                    Box::pin(async move { cb(ExecStream::Stdout, line).await });
                fut
            });
            f
        });
        self.exec(handle, cmd, env, adapter).await
    }

    /// Check if a sandbox is still alive and usable.
    async fn is_alive(&self, handle: &SandboxHandle) -> Result<bool, AgentError>;

    /// Write a file directly into the sandbox at an absolute path.
    ///
    /// Implementations should NOT use `bash -c "cat > ... << EOF"` heredocs —
    /// those go through the exec stream, which has a known phantom-stream
    /// hang on silent processes that produce no output. Use a tar stream
    /// (Docker `put_archive`) or equivalent native API instead.
    ///
    /// `mode` is a Unix mode (e.g. 0o600 for secrets, 0o644 for skill files).
    async fn write_file(
        &self,
        handle: &SandboxHandle,
        path: &str,
        contents: &[u8],
        mode: u32,
    ) -> Result<(), AgentError>;

    /// Read a file from inside the sandbox. Returns the raw bytes.
    ///
    /// Like `write_file`, implementations must NOT go through `bash -c cat`
    /// exec — that path is subject to the bollard phantom-stream hang on
    /// silent processes. Use a native tar download (`download_from_container`)
    /// or equivalent API.
    ///
    /// Returns an error if the file does not exist.
    async fn read_file(&self, handle: &SandboxHandle, path: &str) -> Result<Vec<u8>, AgentError>;

    /// Write an entire local directory tree into the sandbox at `target_path`.
    ///
    /// Builds a single tar archive from `local_dir` and uploads in one shot,
    /// which is much more efficient than calling `write_file` for each entry.
    /// The directory structure under `local_dir` is preserved relative to
    /// `target_path`.
    async fn write_directory(
        &self,
        handle: &SandboxHandle,
        local_dir: &std::path::Path,
        target_path: &str,
    ) -> Result<(), AgentError>;

    /// Kill processes inside the sandbox matching a pgrep/pkill pattern.
    ///
    /// `signal` is constrained to [`KillSignal`] — only SIGTERM/SIGKILL are
    /// valid. `pattern` is passed to `pkill -f` — it matches against the
    /// full command line, so prefer anchored patterns like `^claude ` to
    /// avoid killing unrelated processes.
    ///
    /// Returns Ok(()) whether or not anything was actually killed — the
    /// operation is inherently best-effort.
    async fn kill_processes(
        &self,
        handle: &SandboxHandle,
        pattern: &str,
        signal: KillSignal,
    ) -> Result<(), AgentError>;

    /// Destroy sandbox and clean up its container.
    ///
    /// When `purge_volumes` is true, the provider also removes any per-run
    /// persistent volumes (e.g. the `/home/temps` named volume for Docker).
    /// When false, volumes are left in place so a subsequent `create` with
    /// the same run_id resumes the previous home directory — the workspace
    /// uses this to preserve Claude auth / shell history across a session
    /// close+reopen cycle.
    async fn destroy(&self, handle: &SandboxHandle, purge_volumes: bool) -> Result<(), AgentError>;

    /// Stop a running sandbox without removing it. Default implementation
    /// reports the operation as unsupported so non-Docker backends compile
    /// unchanged. Docker-backed providers override this with a real stop.
    async fn stop(&self, handle: &SandboxHandle) -> Result<(), AgentError> {
        Err(AgentError::SandboxExecFailed {
            run_id: 0,
            sandbox_id: handle.sandbox_id.clone(),
            reason: format!(
                "stop() is not supported by sandbox provider '{}'",
                self.name()
            ),
        })
    }

    /// Start a stopped sandbox. Same default-unsupported policy as `stop`.
    async fn start(&self, handle: &SandboxHandle) -> Result<(), AgentError> {
        Err(AgentError::SandboxExecFailed {
            run_id: 0,
            sandbox_id: handle.sandbox_id.clone(),
            reason: format!(
                "start() is not supported by sandbox provider '{}'",
                self.name()
            ),
        })
    }

    /// Restart a sandbox in place. Default impl chains `stop` then `start`,
    /// so any backend that overrides those automatically gets restart for free.
    async fn restart(&self, handle: &SandboxHandle) -> Result<(), AgentError> {
        self.stop(handle).await?;
        self.start(handle).await
    }

    /// Grow the sandbox's root disk to `new_size_mb`. Only the Firecracker
    /// backend supports this (Docker containers have no fixed disk to
    /// resize). Grow-only — shrinking is rejected. The Firecracker impl
    /// stops the VM, grows the backing ext4 offline, and restarts it, so the
    /// filesystem is resized without any in-guest tooling; the VM reboots
    /// (data persists) rather than resizing fully live. Default: unsupported.
    async fn resize_disk(
        &self,
        handle: &SandboxHandle,
        new_size_mb: u64,
    ) -> Result<(), AgentError> {
        let _ = new_size_mb;
        Err(AgentError::SandboxExecFailed {
            run_id: 0,
            sandbox_id: handle.sandbox_id.clone(),
            reason: format!(
                "resize_disk is not supported by sandbox provider '{}'",
                self.name()
            ),
        })
    }

    /// Attempt to recover a sandbox after server restart (by naming convention).
    /// Returns `None` if no recoverable sandbox exists for this run.
    async fn recover(&self, run_id: i32) -> Result<Option<SandboxHandle>, AgentError>;

    /// Recover a sandbox by its explicit container name. Used by standalone
    /// sandboxes that were created with `container_name_override` — the
    /// numeric `recover(run_id)` path can't find them because the container
    /// is named after a `public_id` rather than the integer. Default impl
    /// returns `None` so providers that don't need this compile unchanged.
    async fn recover_by_name(
        &self,
        _container_name: &str,
    ) -> Result<Option<SandboxHandle>, AgentError> {
        Ok(None)
    }

    /// Whether this provider can create sandboxes on `backend`. Consumers
    /// call this before create so a request for an unprovisioned backend
    /// (e.g. `firecracker` on a Docker-only host) fails with a clear error
    /// instead of silently downgrading. Concrete single-backend providers
    /// answer for their own backend; the routing provider answers for every
    /// backend it has registered. Default is permissive for backwards
    /// compatibility, so every concrete provider overrides it.
    fn supports_backend(&self, backend: SandboxBackend) -> bool {
        let _ = backend;
        true
    }

    /// Provider name for logging and error messages.
    fn name(&self) -> &str;

    /// Check if the sandbox backend is available (e.g. Docker daemon reachable).
    async fn is_available(&self) -> bool;

    /// Check if the sandbox image is built/available.
    /// Returns (is_ready, image_name).
    async fn image_status(&self) -> Result<(bool, String), AgentError>;

    /// Delete and rebuild the sandbox image. Returns the image name.
    async fn rebuild_image(&self) -> Result<String, AgentError>;

    /// Report rootfs storage for the management API. Default: empty —
    /// backends without a rootfs cache (Docker, local) have nothing to show.
    async fn rootfs_report(&self) -> Result<RootfsReport, AgentError> {
        Ok(RootfsReport::default())
    }

    /// Reclaim rootfs cache entries not backing any live sandbox. Default:
    /// a no-op empty report. Safe to call any time — live VMs hold their own
    /// per-VM disks, so evicting an unreferenced cache entry only forces a
    /// reconversion on the next create from that image.
    async fn gc_rootfs(&self) -> Result<RootfsGcReport, AgentError> {
        Ok(RootfsGcReport::default())
    }

    /// Rebuild the image with progress reporting. Each build log line is sent
    /// via `on_progress`. Default implementation delegates to `rebuild_image`
    /// with a single "done" message.
    async fn rebuild_image_with_progress(
        &self,
        on_progress: tokio::sync::mpsc::Sender<String>,
    ) -> Result<String, AgentError> {
        let _ = on_progress.send("Building image...".to_string()).await;
        let result = self.rebuild_image().await;
        match &result {
            Ok(name) => {
                let _ = on_progress.send(format!("Image built: {}", name)).await;
            }
            Err(e) => {
                let _ = on_progress.send(format!("Build failed: {}", e)).await;
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kill_signal_term_is_15() {
        assert_eq!(KillSignal::Term.as_number(), 15);
    }

    #[test]
    fn kill_signal_kill_is_9() {
        assert_eq!(KillSignal::Kill.as_number(), 9);
    }

    #[test]
    fn kill_signal_is_copy() {
        // Guards that KillSignal stays cheap to pass — if someone adds a
        // String payload later, this stops compiling and forces a review.
        let s = KillSignal::Term;
        let a = s;
        let b = s;
        assert_eq!(a.as_number(), b.as_number());
    }

    /// Compile-time assertion that `SandboxProvider` is object-safe — i.e.
    /// `Arc<dyn SandboxProvider>` is legal. Every consumer holds the trait
    /// behind dynamic dispatch; breaking object-safety (by adding a generic
    /// method or a `Self` return type) would cascade through `temps-agents`
    /// and `temps-sandbox`.
    ///
    /// This test does not run any code at runtime — the assertion is that
    /// this function type-checks at all.
    #[test]
    fn compile_asserts_object_safety() {
        fn assert_object_safe(_: &Arc<dyn SandboxProvider>) {}
        // We never actually construct one here — the type check alone is
        // the guard. The closure is never invoked.
        let _ = |p: Arc<dyn SandboxProvider>| assert_object_safe(&p);
    }
}
