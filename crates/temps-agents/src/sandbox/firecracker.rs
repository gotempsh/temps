// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Firecracker microVM sandbox backend (ADR-029).
//!
//! Each sandbox is a KVM microVM: pinned guest kernel, an ext4 rootfs
//! derived from the sandbox's Docker image (Docker is the image toolchain —
//! pull/export happen through bollard, ADR-029 §4), and `temps-vm-agent`
//! injected as PID 1 serving exec/fs RPCs over vsock (§5). Interactive
//! terminals reuse the Docker backend's `temps-pty-agent`: it is injected next
//! to `temps-vm-agent`, supervised by it, and reached through a second vsock
//! listener that relays bytes to the agent's unix socket (§5).
//!
//! On-disk layout under `<data_dir>/firecracker/` (provisioned by
//! `temps firecracker setup`):
//!   bin/{firecracker,jailer,temps-vm-agent,temps-pty-agent}   pinned binaries
//!   kernel/vmlinux-<ver>                      pinned guest kernel
//!   state.json                               setup outcome (smoke_ok gate)
//!   rootfs-cache/<image-digest>.ext4          converted images, digest-keyed
//!   vms/<name>/{rootfs.ext4,vm.json,fc.pid,v.sock,console.log,env.json}
//!
//! Networking: each networked VM gets a TAP off the pool provisioned by
//! `temps firecracker setup`, NAT'd to the internet, with guest→host and
//! cloud-metadata paths dropped and per-port isolation. Egress is otherwise
//! open — scoping it to a credential proxy is ADR-013 (deferred), so
//! `network_mode: "restricted"` currently fails closed to no-network.
//!
//! Still deferred: the jailer (VMM runs as the server's own user — still
//! KVM-isolated), the ADR-013 egress credential proxy, and memory-state-based
//! pause. Persistent snapshots rebuild a fresh ext4 image from the quiesced
//! filesystem so deleted blocks and runtime credential state are excluded.

use async_trait::async_trait;
use futures::StreamExt;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

use temps_vm_agent::{
    Request, Response, SpawnRestartPolicy, AGENT_PORT, MAX_FRAME_BYTES, PTY_PORT, WORK_DIR,
};

/// What [`FirecrackerSandboxProvider::spawn_process`] starts in a guest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GuestSpawn {
    pub cmd: Vec<String>,
    pub env: HashMap<String, String>,
    pub cwd: Option<String>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    /// What the guest agent does when the process exits.
    pub restart: SpawnRestartPolicy,
    /// Size of one log file; `None` takes the guest's bounded default.
    pub log_max_bytes: Option<u64>,
    /// Log files kept (current + rotated); `None` takes the guest's default.
    pub log_max_files: Option<u32>,
    /// Resource ceilings the guest applies to the process.
    pub limits: temps_vm_agent::SpawnLimits,
}

/// What the guest agent reports about a spawned process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GuestProcessStatus {
    pub running: bool,
    pub exit_code: Option<i32>,
    /// Restarts the guest made under the process's restart policy.
    pub restart_count: u32,
    /// Exited, with a restart pending once the backoff has passed.
    pub restarting: bool,
}

/// What one incremental read of the workload log returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkloadLogChunk {
    pub data: String,
    pub cursor: Option<temps_vm_agent::LogCursor>,
    pub skipped: bool,
    pub more: bool,
}

/// Whether an agent `Err` says it does not know the request (an agent built
/// before that request existed), as opposed to the request failing.
fn is_unknown_request(message: &str) -> bool {
    message.starts_with("bad request:") && message.contains("unknown variant")
}

use super::{
    KillSignal, PtyAttachment, SandboxCreateConfig, SandboxExecResult, SandboxHandle,
    SandboxProvider,
};
use crate::ai_cli::OnEventCallback;
use crate::error::AgentError;

/// VM name prefix — the routing provider dispatches handles on this.
pub const FC_SANDBOX_NAME_PREFIX: &str = "temps-fcsandbox-";

/// Image used when a sandbox doesn't specify one. Small, has a shell —
/// good enough until the temps runtime images grow Firecracker variants.
const DEFAULT_IMAGE: &str = "alpine:3.20";

/// Default per-VM root disk when the request doesn't specify one (MiB).
const DEFAULT_DISK_MB: u64 = 1024;
/// Slack added over the image's content size when sizing the cached ext4,
/// leaving room for the journal + inode tables + a little working space.
/// The per-VM disk is then grown from this base to the requested size.
const CACHE_SLACK_MB: u64 = 64;

const AGENT_READY_TIMEOUT: Duration = Duration::from_secs(15);
/// How long opening a terminal stream may take (socket connect plus the
/// hybrid-vsock handshake). The guest answers in milliseconds; this only
/// bounds a wedged VMM.
const PTY_ATTACH_TIMEOUT: Duration = Duration::from_secs(10);
/// Bytes read per chunk from a terminal stream. One buffer per attachment.
const PTY_READ_CHUNK_BYTES: usize = 16 * 1024;
const RPC_TIMEOUT: Duration = Duration::from_secs(300);
const SHUTDOWN_GRACE: Duration = Duration::from_secs(8);
const SNAPSHOT_EXTRACTION_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// How much of the extractor's stderr is kept for the failure message. The
/// extractor parses an untrusted guest image, so its output is bounded rather
/// than trusted to be short.
const SNAPSHOT_EXTRACTOR_STDERR_BYTES: usize = 2048;
const MIN_SNAPSHOT_STAGING_BYTES: u64 = CACHE_SLACK_MB * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
enum SnapshotExtractorError {
    #[error("failed to start sandboxed debugfs: {0}")]
    Start(#[source] std::io::Error),
    #[error("failed while waiting for sandboxed debugfs: {0}")]
    Wait(#[source] std::io::Error),
    #[error("sandboxed debugfs exceeded its {timeout_seconds} second timeout")]
    TimedOut { timeout_seconds: u64 },
}

/// What a bubblewrap start failure usually means on a host that passed
/// `temps firecracker setup`, from the tool's stderr; empty for anything else.
fn bubblewrap_hint(stderr: &str) -> &'static str {
    if stderr.contains("Unexpected capabilities but not setuid") {
        "; bubblewrap refuses to start while the server holds capabilities (a systemd unit \
         with AmbientCapabilities=). Upgrade Temps, which clears them for this helper"
    } else if stderr.contains("No permissions to create new namespace") {
        "; the host forbids bubblewrap an unprivileged user namespace (Ubuntu 24.04+ \
         sets kernel.apparmor_restrict_unprivileged_userns=1). Allow it for bwrap with an \
         AppArmor profile, or set that sysctl to 0"
    } else {
        ""
    }
}

/// Longest a filesystem check or resize of one guest image may take.
const EXT4_TOOL_TIMEOUT: Duration = Duration::from_secs(600);

/// Failure text for a non-zero extractor exit: the status, what the tool said
/// (it is the only evidence of *why*), and the cause that status most often
/// means on a host that passed `temps firecracker setup`.
fn snapshot_export_failure_message(status: std::process::ExitStatus, stderr: &str) -> String {
    let said = if stderr.is_empty() {
        "it printed nothing".to_string()
    } else {
        format!("it said: {stderr}")
    };
    let hint = bubblewrap_hint(stderr);
    format!("sandboxed debugfs snapshot export failed with status {status}; {said}{hint}")
}

#[derive(Debug)]
struct SnapshotExtractorOutput {
    status: std::process::ExitStatus,
    /// Bounded head of the extractor's stderr; empty when it printed nothing.
    stderr: String,
}

struct FuseSnapshotStaging {
    mount_point: PathBuf,
    backing_file: PathBuf,
    mounted: bool,
}

impl FuseSnapshotStaging {
    fn is_disconnected_mount_error(error: &std::io::Error) -> bool {
        matches!(error.raw_os_error(), Some(libc::ENOTCONN) | Some(libc::EIO))
    }

    fn path_is_mountpoint(path: &Path) -> std::io::Result<bool> {
        use std::os::unix::fs::MetadataExt;

        let Some(parent) = path.parent() else {
            return Ok(false);
        };
        Ok(std::fs::metadata(path)?.dev() != std::fs::metadata(parent)?.dev())
    }

    fn path_requires_unmount(path: &Path) -> std::io::Result<bool> {
        match std::fs::symlink_metadata(path) {
            Ok(_) => match Self::path_is_mountpoint(path) {
                Ok(mounted) => Ok(mounted),
                Err(error) if Self::is_disconnected_mount_error(&error) => Ok(true),
                Err(error) => Err(error),
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) if Self::is_disconnected_mount_error(&error) => Ok(true),
            Err(error) => Err(error),
        }
    }

    async fn unmount(path: &Path, lazy: bool) -> std::io::Result<()> {
        let mut command = tokio::process::Command::new("fusermount3");
        command.arg("-u");
        if lazy {
            command.arg("-z");
        }
        let output = command.arg(path).output().await?;
        if output.status.success() {
            Ok(())
        } else {
            Err(std::io::Error::other(format!(
                "fusermount3 failed with status {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            )))
        }
    }

    async fn cleanup(mut self) -> std::io::Result<()> {
        Self::unmount(&self.mount_point, false).await?;
        self.mounted = false;

        match tokio::fs::remove_dir_all(&self.mount_point).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        match tokio::fs::remove_file(&self.backing_file).await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
}

impl Drop for FuseSnapshotStaging {
    fn drop(&mut self) {
        let unmounted = if self.mounted {
            std::process::Command::new("fusermount3")
                .args(["-u", "-z"])
                .arg(&self.mount_point)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|status| status.success())
        } else {
            true
        };
        if unmounted {
            // These remove only the private empty mount point and its bounded
            // scratch image. They never walk guest contents.
            let _ = std::fs::remove_dir(&self.mount_point);
            let _ = std::fs::remove_file(&self.backing_file);
        }
    }
}

#[derive(Clone)]
pub struct FirecrackerSandboxConfig {
    /// Temps data directory (`$TEMPS_DATA_DIR` / `~/.temps`).
    pub data_dir: PathBuf,
    pub default_vcpus: u32,
    pub default_memory_mib: u64,
    /// Root disk size (MiB) for sandboxes that don't request one.
    pub default_disk_mb: u64,
}

impl FirecrackerSandboxConfig {
    pub fn from_data_dir(data_dir: PathBuf) -> Self {
        Self {
            data_dir,
            default_vcpus: 1,
            default_memory_mib: 512,
            default_disk_mb: DEFAULT_DISK_MB,
        }
    }

    pub(crate) fn fc_root(&self) -> PathBuf {
        self.data_dir.join("firecracker")
    }
    fn firecracker_bin(&self) -> PathBuf {
        self.fc_root().join("bin/firecracker")
    }
    pub(crate) fn agent_bin(&self) -> PathBuf {
        self.fc_root().join("bin/temps-vm-agent")
    }
    /// Static musl `temps-pty-agent`, injected next to the VM agent so the
    /// guest can serve interactive terminals. Optional: a host without it
    /// still creates sandboxes, just without terminals.
    pub(crate) fn pty_agent_bin(&self) -> PathBuf {
        self.fc_root().join("bin/temps-pty-agent")
    }
    fn kernel_glob_dir(&self) -> PathBuf {
        self.fc_root().join("kernel")
    }
    fn cache_dir(&self) -> PathBuf {
        self.fc_root().join("rootfs-cache")
    }
    fn vms_dir(&self) -> PathBuf {
        self.fc_root().join("vms")
    }
    fn vm_dir(&self, name: &str) -> PathBuf {
        self.vms_dir().join(name)
    }
}

/// What a VMM-process check plus an agent ping say about a VM. No VMM process
/// is proof the VM is down. A VMM that is running but whose guest agent missed
/// a ping is *not* proof of anything (a VM that is booting, swapping or
/// saturated misses pings), so it is an error, not `false`: callers that
/// act on "not running" (the row reconciliation) must not act on it.
fn liveness_verdict(vmm_running: bool, agent_answered: bool) -> Result<bool, &'static str> {
    match (vmm_running, agent_answered) {
        (false, _) => Ok(false),
        (true, true) => Ok(true),
        (true, false) => Err(
            "the VMM process is running but its guest agent did not answer a ping within 3s \
             (the VM may still be booting or be overloaded)",
        ),
    }
}

pub struct FirecrackerSandboxProvider {
    config: FirecrackerSandboxConfig,
    docker: Arc<bollard::Docker>,
    /// Serializes rootfs conversion per image digest.
    cache_lock: tokio::sync::Mutex<()>,
    /// Serializes TAP-pool allocation (backed by `taps.json`).
    tap_lock: tokio::sync::Mutex<()>,
}

/// Host networking facts recorded by `temps firecracker setup` in
/// `state.json`. `tap_count == 0` means the root network stage never ran —
/// VMs can still boot, but only with `network_mode: "none"`.
struct NetState {
    gateway: std::net::Ipv4Addr,
    prefix: u32,
    tap_count: u32,
}

impl NetState {
    fn netmask(&self) -> std::net::Ipv4Addr {
        std::net::Ipv4Addr::from(u32::MAX << (32 - self.prefix))
    }

    /// Guest IP for a TAP index. `.10+` leaves room for the gateway and
    /// future infrastructure addresses.
    fn guest_ip(&self, tap_index: u32) -> std::net::Ipv4Addr {
        std::net::Ipv4Addr::from(
            u32::from(self.gateway) & (u32::MAX << (32 - self.prefix)) | (10 + tap_index),
        )
    }
}

impl FirecrackerSandboxProvider {
    pub fn new(config: FirecrackerSandboxConfig, docker: Arc<bollard::Docker>) -> Self {
        Self {
            config,
            docker,
            cache_lock: tokio::sync::Mutex::new(()),
            tap_lock: tokio::sync::Mutex::new(()),
        }
    }

    // ── TAP pool (persistent devices created by setup's network stage) ──

    fn net_state(&self) -> Option<NetState> {
        let state: serde_json::Value =
            serde_json::from_slice(&std::fs::read(self.config.fc_root().join("state.json")).ok()?)
                .ok()?;
        let subnet = state["subnet"].as_str()?;
        let (addr, prefix) = subnet.split_once('/')?;
        Some(NetState {
            gateway: addr.parse().ok()?,
            prefix: prefix.parse().ok().filter(|p| (8..=30).contains(p))?,
            tap_count: state["tap_count"].as_u64().unwrap_or(0) as u32,
        })
    }

    fn taps_file(&self) -> PathBuf {
        self.config.fc_root().join("taps.json")
    }

    fn read_taps(&self) -> HashMap<u32, String> {
        std::fs::read(self.taps_file())
            .ok()
            .and_then(|d| serde_json::from_slice(&d).ok())
            .unwrap_or_default()
    }

    fn write_taps(&self, taps: &HashMap<u32, String>) -> Result<(), AgentError> {
        std::fs::write(
            self.taps_file(),
            serde_json::to_vec_pretty(taps).map_err(|e| self.err("-", e))?,
        )?;
        Ok(())
    }

    /// Claim a free TAP for `name`. Idempotent per VM name (start-after-stop
    /// reuses the sandbox's existing claim, keeping its IP stable).
    async fn allocate_tap(&self, name: &str, net: &NetState) -> Result<u32, AgentError> {
        let _guard = self.tap_lock.lock().await;
        let mut taps = self.read_taps();
        if let Some((&idx, _)) = taps.iter().find(|(_, owner)| owner.as_str() == name) {
            return Ok(idx);
        }
        let idx = (0..net.tap_count)
            .find(|i| {
                !taps.contains_key(i)
                    && Path::new("/sys/class/net")
                        .join(format!("temps-fc-tap{}", i))
                        .exists()
            })
            .ok_or_else(|| {
                self.err(
                    name,
                    format!(
                        "no free TAP device (pool of {}; increase with \
                         `sudo temps firecracker setup --network-only --tap-count N`)",
                        net.tap_count
                    ),
                )
            })?;
        taps.insert(idx, name.to_string());
        self.write_taps(&taps)?;
        Ok(idx)
    }

    async fn release_tap(&self, name: &str) {
        let _guard = self.tap_lock.lock().await;
        let mut taps = self.read_taps();
        taps.retain(|_, owner| owner != name);
        let _ = self.write_taps(&taps);
    }

    /// [`SandboxProvider::attach_pty`] with an explicit handshake deadline,
    /// so the timeout path is testable without waiting out the real one.
    ///
    /// A guest that does not answer in time is a
    /// [`AgentError::SandboxTerminalTimeout`], not an exec failure: it is
    /// stalled rather than missing terminal support, and callers answer the
    /// two differently.
    async fn attach_pty_within(
        &self,
        handle: &SandboxHandle,
        handshake_timeout: Duration,
    ) -> Result<PtyAttachment, AgentError> {
        let name = handle.sandbox_name.as_str();
        let sock = self.config.vm_dir(name).join("v.sock");

        if !sock.exists() {
            return Err(self.err(
                name,
                format!(
                    "cannot attach a terminal: sandbox is not running (no vsock socket at {})",
                    sock.display()
                ),
            ));
        }

        let stream =
            match tokio::time::timeout(handshake_timeout, vsock_connect(&sock, PTY_PORT)).await {
                Ok(Ok(stream)) => stream,
                Ok(Err(e)) => {
                    // The VMM drops the connection when the guest has nothing
                    // listening on the port, so a handshake failure here almost
                    // always means this guest was built without the PTY agent.
                    let terminal = firecracker_terminal_capability_for(&self.config);
                    let remedy = if terminal.supported {
                        "this sandbox was created before terminal support, or from a \
                     snapshot taken before it; recreate the sandbox to get it"
                            .to_string()
                    } else {
                        format!(
                            "{} — run `{}`, then recreate the sandbox",
                            terminal
                                .reason
                                .as_deref()
                                .unwrap_or("the guest terminal agent is unusable"),
                            PTY_AGENT_SETUP_COMMAND
                        )
                    };
                    tracing::warn!(
                        sandbox = %name,
                        vsock_port = PTY_PORT,
                        error = %e,
                        "firecracker terminal attach failed"
                    );
                    return Err(self.err(
                        name,
                        format!(
                            "no terminal listener answered on vsock port {} ({}): {}",
                            PTY_PORT, e, remedy
                        ),
                    ));
                }
                Err(_) => {
                    tracing::warn!(
                        sandbox = %name,
                        vsock_port = PTY_PORT,
                        timeout_secs = handshake_timeout.as_secs(),
                        "firecracker terminal handshake timed out"
                    );
                    return Err(AgentError::terminal_timeout(
                        name,
                        format!("agent handshake on vsock port {}", PTY_PORT),
                        handshake_timeout,
                    ));
                }
            };

        let (reader, writer) = stream.into_split();
        let sandbox_id = handle.sandbox_id.clone();
        // `None` after an error so a failed read ends the stream instead of
        // being retried in a loop.
        let output = futures::stream::unfold(Some(reader), move |state| {
            let sandbox_id = sandbox_id.clone();
            async move {
                let mut reader = state?;
                let mut buf = vec![0u8; PTY_READ_CHUNK_BYTES];
                match reader.read(&mut buf).await {
                    Ok(0) => None,
                    Ok(n) => {
                        buf.truncate(n);
                        Some((Ok(bytes::Bytes::from(buf)), Some(reader)))
                    }
                    Err(e) => Some((
                        Err(AgentError::SandboxExecFailed {
                            run_id: 0,
                            sandbox_id,
                            reason: format!("terminal stream read failed: {}", e),
                        }),
                        None,
                    )),
                }
            }
        });
        Ok(PtyAttachment {
            output: Box::pin(output),
            input: Box::pin(writer),
        })
    }

    fn err(&self, sandbox_id: &str, reason: impl std::fmt::Display) -> AgentError {
        AgentError::SandboxExecFailed {
            run_id: 0,
            sandbox_id: sandbox_id.to_string(),
            reason: reason.to_string(),
        }
    }

    /// The single pinned kernel installed by `temps firecracker setup`.
    fn kernel_path(&self) -> Result<PathBuf, AgentError> {
        let dir = self.config.kernel_glob_dir();
        let entry = std::fs::read_dir(&dir)
            .ok()
            .and_then(|mut entries| {
                entries.find_map(|e| {
                    let e = e.ok()?;
                    e.file_name()
                        .to_string_lossy()
                        .starts_with("vmlinux-")
                        .then(|| e.path())
                })
            })
            .ok_or_else(|| {
                self.err(
                    "-",
                    format!(
                        "no guest kernel under {} — run `temps firecracker setup`",
                        dir.display()
                    ),
                )
            })?;
        Ok(entry)
    }

    fn resolve_name(&self, config: &SandboxCreateConfig) -> String {
        match &config.container_name_override {
            Some(id) => format!("{}{}", FC_SANDBOX_NAME_PREFIX, id),
            None => format!("{}{}", FC_SANDBOX_NAME_PREFIX, config.run_id),
        }
    }

    fn handle_for(&self, name: &str) -> SandboxHandle {
        self.handle_with_image(name, String::new())
    }

    fn handle_with_image(&self, name: &str, image: String) -> SandboxHandle {
        SandboxHandle {
            node_id: None,
            sandbox_id: name.to_string(),
            sandbox_name: name.to_string(),
            work_dir: PathBuf::from(WORK_DIR),
            backend: super::SandboxBackend::Firecracker,
            image,
            conditions: Vec::new(),
        }
    }

    /// Fingerprint of the guest binaries injected into every rootfs.
    ///
    /// Part of the rootfs cache key: the agents are baked into the cached
    /// ext4, so a cache entry converted before `temps firecracker setup`
    /// staged a newer (or the first) `temps-pty-agent` must not be reused —
    /// it would boot VMs that have no terminal support no matter what the
    /// host has staged now.
    fn guest_agent_fingerprint(&self) -> Result<String, AgentError> {
        use sha2::{Digest, Sha256};

        let vm_agent_path = self.config.agent_bin();
        let vm_agent = Self::hash_file(&vm_agent_path).map_err(|e| {
            self.err(
                "-",
                format!(
                    "guest agent missing or unreadable at {} ({}) — run `temps firecracker setup`",
                    vm_agent_path.display(),
                    e
                ),
            )
        })?;
        // The terminal agent is optional, so nothing about it may fail
        // sandbox creation. What is keyed is what will actually be injected:
        // only an agent `ensure_rootfs_locked` accepts counts, and a missing,
        // unreadable or unusable one is "absent" — the rootfs is converted
        // without it and terminals report why they are unavailable.
        let pty_agent_path = self.config.pty_agent_bin();
        let terminal = firecracker_terminal_capability_for(&self.config);
        let pty_agent = if terminal.supported {
            match Self::hash_file(&pty_agent_path) {
                Ok(hash) => hash,
                Err(e) => {
                    tracing::warn!(
                        pty_agent = %pty_agent_path.display(),
                        error = %e,
                        "terminal agent became unreadable while keying the rootfs cache; \
                         converting without it"
                    );
                    "absent".to_string()
                }
            }
        } else {
            "absent".to_string()
        };
        let digest = Sha256::digest(format!("vm:{vm_agent}\npty:{pty_agent}").as_bytes());
        Ok(hex::encode(&digest[..6]))
    }

    // ── Rootfs conversion (ADR-029 §4, digest-keyed cache) ──────────

    /// Docker image → ext4 rootfs with the agent injected. Returns the
    /// cached artifact path, converting on first use per image digest.
    ///
    /// The caller MUST hold `cache_lock` — this serializes conversions and,
    /// crucially, lets `create` keep the lock through the per-VM copy + the
    /// reference marker so `gc_rootfs` (which also takes `cache_lock`) can't
    /// delete a freshly-converted entry before it's referenced.
    async fn ensure_rootfs_locked(&self, image: &str) -> Result<PathBuf, AgentError> {
        // Pull if missing, then resolve the digest-stable image id.
        let inspect = match self.docker.inspect_image(image).await {
            Ok(i) => i,
            Err(_) => {
                self.pull_image(image).await?;
                self.docker
                    .inspect_image(image)
                    .await
                    .map_err(|e| self.err("-", format!("inspect {}: {}", image, e)))?
            }
        };
        let image_id = inspect
            .id
            .ok_or_else(|| self.err("-", format!("image {} has no id", image)))?;
        let cache_key = format!(
            "{}-g{}",
            image_id.replace(':', "-"),
            self.guest_agent_fingerprint()?
        );
        let cached = self.config.cache_dir().join(format!("{}.ext4", cache_key));
        if cached.exists() {
            return Ok(cached);
        }

        tracing::info!("converting {} ({}) to Firecracker rootfs", image, image_id);
        std::fs::create_dir_all(self.config.cache_dir())?;
        let staging = self
            .config
            .cache_dir()
            .join(format!("{}.staging", cache_key));
        let _ = std::fs::remove_dir_all(&staging);
        std::fs::create_dir_all(&staging)?;

        // Materialize the image filesystem via container export.
        let container = self
            .docker
            .create_container(
                None::<bollard::query_parameters::CreateContainerOptions>,
                bollard::models::ContainerCreateBody {
                    image: Some(image.to_string()),
                    cmd: Some(vec!["true".to_string()]),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| self.err("-", format!("create container for {}: {}", image, e)))?;
        // Stream the export to a temp tar on disk (bounded memory, vs.
        // buffering the whole image), then do the CPU/IO-heavy extraction
        // off the async runtime.
        let tar_path = staging.join("export.tar");
        {
            let mut file = tokio::fs::File::create(&tar_path)
                .await
                .map_err(|e| self.err("-", format!("export tar create: {}", e)))?;
            let mut export = self.docker.export_container(&container.id);
            while let Some(chunk) = export.next().await {
                let chunk = chunk.map_err(|e| self.err("-", format!("export: {}", e)))?;
                file.write_all(&chunk)
                    .await
                    .map_err(|e| self.err("-", format!("export write: {}", e)))?;
            }
            let _ = file.flush().await;
        }
        let _ = self
            .docker
            .remove_container(
                &container.id,
                Some(bollard::query_parameters::RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await;

        let rootfs_dir = staging.join("rootfs");
        // `guest_agent_fingerprint` above already proved the VM agent is
        // readable; the PTY agent is optional.
        let agent_src = self.config.agent_bin();
        // Inject only an agent the guest can run. A file that exists but is
        // for another CPU or needs a loader would otherwise be baked into the
        // cached rootfs and fail at the first terminal.
        let terminal = firecracker_terminal_capability_for(&self.config);
        let pty_agent_src = terminal.supported.then(|| self.config.pty_agent_bin());
        if pty_agent_src.is_none() {
            tracing::warn!(
                pty_agent = %self.config.pty_agent_bin().display(),
                reason = terminal.reason.as_deref().unwrap_or("unknown"),
                "terminal agent not injected; sandboxes converted now will not support \
                 interactive terminals — run `{}`",
                PTY_AGENT_SETUP_COMMAND
            );
        }
        let workdir_rel = WORK_DIR.trim_start_matches('/').to_string();

        // Blocking: unprivileged extraction (skip device nodes — the agent
        // mounts devtmpfs at boot; `unpack_in` sanitizes path traversal),
        // scrub Docker-injected artifacts (`/.dockerenv` fools is-docker
        // probes; the network files hold Docker's embedded-DNS 127.0.0.11
        // which doesn't exist in the VM and the agent rewrites at boot), and
        // inject the guest agent. Returns the extracted content size.
        let rootfs_for_task = rootfs_dir.clone();
        let content_bytes = tokio::task::spawn_blocking(move || -> Result<u64, String> {
            std::fs::create_dir_all(&rootfs_for_task).map_err(|e| e.to_string())?;
            let tar_file = std::fs::File::open(&tar_path).map_err(|e| e.to_string())?;
            let report = super::image_archive::extract_image_archive(
                std::io::BufReader::new(tar_file),
                &tar_path,
                &rootfs_for_task,
                super::image_archive::IMAGE_ARCHIVE_LIMITS,
            )
            .map_err(|e| e.to_string())?;
            if report.skipped > 0 {
                tracing::debug!(
                    entries = report.entries,
                    skipped = report.skipped,
                    "image archive entries left out (device nodes, FIFOs, parent paths)"
                );
            }
            let _ = std::fs::remove_file(&tar_path);
            // Everything below touches paths *the image controls*: a custom
            // image may make `etc`, `sbin` or the agent files themselves
            // symlinks (absolute ones included) to host paths. Plain
            // `join` + `remove_file`/`copy` would follow them out of the
            // rootfs and delete or overwrite host files, so every path is
            // resolved the way a chroot would and files are created
            // exclusively (see `rootfs_resolve`).
            for artifact in [".dockerenv", "etc/resolv.conf", "etc/hostname", "etc/hosts"] {
                let _ = rootfs_remove(&rootfs_for_task, Path::new(artifact));
            }
            rootfs_inject(
                &rootfs_for_task,
                Path::new("sbin/temps-vm-agent"),
                &agent_src,
            )
            .map_err(|e| e.to_string())?;
            if let Some(pty_src) = &pty_agent_src {
                // Same directory as the VM agent; the staged file's
                // executable bit is kept. The guest's PID 1 re-asserts root
                // ownership of it at every boot (the rootfs is built
                // unprivileged, so the file carries the host user's uid)
                // before supervising it.
                rootfs_inject(&rootfs_for_task, Path::new("sbin/temps-pty-agent"), pty_src)
                    .map_err(|e| e.to_string())?;
            }
            let workdir = rootfs_resolve(&rootfs_for_task, Path::new(&workdir_rel))
                .map_err(|e| e.to_string())?;
            std::fs::create_dir_all(&workdir).map_err(|e| e.to_string())?;
            Ok(dir_size(&rootfs_for_task))
        })
        .await
        .map_err(|e| self.err("-", format!("extraction task: {}", e)))?
        .map_err(|e| self.err("-", format!("extraction: {}", e)))?;

        // Size the cache to the image content + slack — the smallest the fs
        // can be, and the floor for any per-VM disk. Each sandbox grows its
        // own copy from here (see `create`), so the cache stays minimal.
        //
        // `lazy_itable_init=0` + `lazy_journal_init=0` write the inode tables
        // and journal at mkfs time (into the sparse staging file, so they
        // cost nothing on disk) instead of letting the guest kernel's
        // ext4lazyinit thread scribble across the whole device on first
        // mount — which was inflating every per-VM copy to the full size.
        let base_bytes =
            ((content_bytes * 3 / 2) + CACHE_SLACK_MB * 1024 * 1024).next_multiple_of(4096);
        let base_blocks = base_bytes / 4096;
        let img_tmp = staging.join("rootfs.ext4");
        let out = tokio::process::Command::new("mkfs.ext4")
            .args([
                "-q",
                "-F",
                "-b",
                "4096",
                "-E",
                "lazy_itable_init=0,lazy_journal_init=0",
                "-d",
            ])
            .arg(&rootfs_dir)
            .arg(&img_tmp)
            .arg(base_blocks.to_string())
            .output()
            .await?;
        if !out.status.success() {
            return Err(self.err(
                "-",
                format!("mkfs.ext4: {}", String::from_utf8_lossy(&out.stderr)),
            ));
        }
        std::fs::rename(&img_tmp, &cached)?;
        let _ = std::fs::remove_dir_all(&staging);
        tracing::info!("rootfs cached at {}", cached.display());
        Ok(cached)
    }

    async fn pull_image(&self, image: &str) -> Result<(), AgentError> {
        let mut pull = self.docker.create_image(
            Some(bollard::query_parameters::CreateImageOptions {
                from_image: Some(image.to_string()),
                ..Default::default()
            }),
            None,
            None,
        );
        while let Some(item) = pull.next().await {
            item.map_err(|e| self.err("-", format!("pull {}: {}", image, e)))?;
        }
        Ok(())
    }

    // ── VM process lifecycle ────────────────────────────────────────

    async fn spawn_vm(&self, name: &str) -> Result<(), AgentError> {
        let vm_dir = self.config.vm_dir(name);
        // Stale hybrid-vsock socket blocks Firecracker from binding.
        let _ = std::fs::remove_file(vm_dir.join("v.sock"));

        let console = std::fs::File::create(vm_dir.join("console.log"))?;
        let child = tokio::process::Command::new(self.config.firecracker_bin())
            .arg("--no-api")
            .arg("--config-file")
            .arg("vm.json")
            .current_dir(&vm_dir)
            .stdin(std::process::Stdio::null())
            .stdout(console.try_clone()?)
            .stderr(console)
            .spawn()
            .map_err(|e| self.err(name, format!("spawn firecracker: {}", e)))?;
        let pid = child
            .id()
            .ok_or_else(|| self.err(name, "firecracker exited immediately"))?;
        std::fs::write(vm_dir.join("fc.pid"), pid.to_string())?;
        // Detach: lifecycle is managed via pid file + vsock, and the VMM
        // must survive this async task. Reaping is the OS's job (server
        // isn't PID 1); stale pids are handled by `vm_pid` liveness probes.
        tokio::spawn(async move {
            let _ = child.wait_with_output().await;
        });

        // Gate readiness on the agent, not the VMM process: boot is fast
        // but "process running" says nothing about PID 1 being up.
        let deadline = tokio::time::Instant::now() + AGENT_READY_TIMEOUT;
        loop {
            match self.rpc(name, &Request::Ping, Duration::from_secs(2)).await {
                Ok(Response::Pong) => return Ok(()),
                _ if tokio::time::Instant::now() > deadline => {
                    let tail = std::fs::read_to_string(vm_dir.join("console.log"))
                        .unwrap_or_default()
                        .lines()
                        .rev()
                        .take(6)
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                        .collect::<Vec<_>>()
                        .join(" | ");
                    return Err(self.err(
                        name,
                        format!(
                            "agent not ready within {:?}; console tail: {}",
                            AGENT_READY_TIMEOUT, tail
                        ),
                    ));
                }
                _ => tokio::time::sleep(Duration::from_millis(100)).await,
            }
        }
    }

    fn vm_pid(&self, name: &str) -> Option<u32> {
        let pid: u32 = std::fs::read_to_string(self.config.vm_dir(name).join("fc.pid"))
            .ok()?
            .trim()
            .parse()
            .ok()?;
        // Liveness + identity: pid recycling must not make a random process
        // look like our VMM.
        let comm = std::fs::read_to_string(format!("/proc/{}/comm", pid)).ok()?;
        comm.trim().starts_with("firecracker").then_some(pid)
    }

    // ── Workload VMs (ADR-055 Phase 10: deployments in a microVM) ──

    /// The VM name the provider gives a `container_name_override`.
    pub(crate) fn vm_name_for(&self, id: &str) -> String {
        format!("{}{}", FC_SANDBOX_NAME_PREFIX, id)
    }

    /// The VM's directory on the host (rootfs, sockets, persisted state).
    pub(crate) fn vm_dir_of(&self, name: &str) -> PathBuf {
        self.config.vm_dir(name)
    }

    /// Every VM directory under the provider's root.
    pub(crate) fn vm_names(&self) -> Vec<String> {
        std::fs::read_dir(self.config.vms_dir())
            .map(|entries| {
                entries
                    .flatten()
                    .map(|entry| entry.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The variables the VM was created with (its 0600 `env.json`).
    pub(crate) fn workload_env(&self, name: &str) -> HashMap<String, String> {
        self.base_env(name)
    }

    /// Whether the VM's VMM process runs.
    pub(crate) fn vm_running(&self, name: &str) -> bool {
        self.vm_pid(name).is_some()
    }

    /// The guest's IP on its TAP device, when it holds one.
    pub(crate) fn guest_ip_of(&self, name: &str) -> Option<std::net::Ipv4Addr> {
        let net = self.net_state()?;
        let (idx, _) = self
            .read_taps()
            .into_iter()
            .find(|(_, owner)| owner.as_str() == name)?;
        Some(net.guest_ip(idx))
    }

    /// Start a process in the guest without waiting for it; returns the pid
    /// that names it from then on (across the guest's restarts of it). The
    /// guest agent bounds and rotates its output and restarts it per
    /// `spawn.restart`.
    pub(crate) async fn spawn_process(
        &self,
        name: &str,
        spawn: GuestSpawn,
    ) -> Result<u32, AgentError> {
        let response = self
            .rpc(
                name,
                &Request::Spawn {
                    cmd: spawn.cmd,
                    env: spawn.env,
                    cwd: spawn.cwd,
                    uid: spawn.uid,
                    gid: spawn.gid,
                    log_path: None,
                    restart: spawn.restart,
                    log_max_bytes: spawn.log_max_bytes,
                    log_max_files: spawn.log_max_files,
                    limits: spawn.limits,
                },
                Duration::from_secs(30),
            )
            .await?;
        match response {
            Response::Spawned { pid } => Ok(pid),
            Response::Err { message } => Err(self.err(name, message)),
            other => Err(self.err(name, format!("unexpected agent response: {other:?}"))),
        }
    }

    /// What the guest knows of the process `pid` started.
    pub(crate) async fn process_status(
        &self,
        name: &str,
        pid: u32,
    ) -> Result<GuestProcessStatus, AgentError> {
        match self
            .rpc(
                name,
                &Request::ProcessStatus { pid },
                Duration::from_secs(5),
            )
            .await?
        {
            Response::ProcessStatus {
                running,
                exit_code,
                restart_count,
                restarting,
            } => Ok(GuestProcessStatus {
                running,
                exit_code,
                restart_count,
                restarting,
            }),
            Response::Err { message } => Err(self.err(name, message)),
            other => Err(self.err(name, format!("unexpected agent response: {other:?}"))),
        }
    }

    /// The last `lines` lines of the workload log, read by the guest agent
    /// across its rotated files. `None` when the guest's agent predates the
    /// `read_log` request (the caller falls back to `tail`).
    pub(crate) async fn read_workload_log(
        &self,
        name: &str,
        lines: u32,
    ) -> Result<Option<String>, AgentError> {
        match self
            .rpc(
                name,
                &Request::ReadLog {
                    path: None,
                    tail_lines: lines,
                },
                Duration::from_secs(30),
            )
            .await?
        {
            Response::Log { data } => Ok(Some(data)),
            Response::Err { message } if is_unknown_request(&message) => Ok(None),
            Response::Err { message } => Err(self.err(name, message)),
            other => Err(self.err(name, format!("unexpected agent response: {other:?}"))),
        }
    }

    /// The complete log lines the guest wrote after `cursor` (from the start
    /// of what it still retains when `None`). `None` when the guest's agent
    /// predates the `read_log_since` request.
    pub(crate) async fn read_workload_log_since(
        &self,
        name: &str,
        cursor: Option<temps_vm_agent::LogCursor>,
    ) -> Result<Option<WorkloadLogChunk>, AgentError> {
        match self
            .rpc(
                name,
                &Request::ReadLogSince {
                    path: None,
                    cursor,
                    max_bytes: None,
                    include_partial: false,
                },
                Duration::from_secs(30),
            )
            .await?
        {
            Response::LogChunk {
                data,
                cursor,
                skipped,
                more,
            } => Ok(Some(WorkloadLogChunk {
                data,
                cursor,
                skipped,
                more,
            })),
            Response::Err { message } if is_unknown_request(&message) => Ok(None),
            Response::Err { message } => Err(self.err(name, message)),
            other => Err(self.err(name, format!("unexpected agent response: {other:?}"))),
        }
    }

    /// One sample of the guest's counters, with CPU and memory taken from
    /// the tree of the spawned process `pid` (every process when `None`).
    /// `None` when the guest's agent predates the `read_stats` request.
    pub(crate) async fn read_workload_stats(
        &self,
        name: &str,
        pid: Option<u32>,
    ) -> Result<Option<temps_vm_agent::stats::GuestStats>, AgentError> {
        match self
            .rpc(name, &Request::ReadStats { pid }, Duration::from_secs(10))
            .await?
        {
            Response::Stats(stats) => Ok(Some(stats)),
            Response::Err { message } if is_unknown_request(&message) => Ok(None),
            Response::Err { message } => Err(self.err(name, message)),
            other => Err(self.err(name, format!("unexpected agent response: {other:?}"))),
        }
    }

    // ── Vsock RPC client (hybrid Unix socket, one RPC per connection) ──

    async fn rpc(
        &self,
        name: &str,
        request: &Request,
        timeout: Duration,
    ) -> Result<Response, AgentError> {
        let sock = self.config.vm_dir(name).join("v.sock");
        let fut = async {
            let mut stream = vsock_connect(&sock, AGENT_PORT).await?;
            let payload = serde_json::to_vec(request).map_err(std::io::Error::other)?;
            stream
                .write_all(&(payload.len() as u32).to_be_bytes())
                .await?;
            stream.write_all(&payload).await?;
            let len = stream.read_u32().await?;
            if len == 0 || len > MAX_FRAME_BYTES {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "response frame out of bounds",
                ));
            }
            let mut buf = vec![0u8; len as usize];
            stream.read_exact(&mut buf).await?;
            serde_json::from_slice::<Response>(&buf).map_err(std::io::Error::other)
        };
        match tokio::time::timeout(timeout, fut).await {
            Ok(Ok(resp)) => Ok(resp),
            Ok(Err(e)) => Err(self.err(name, format!("vsock rpc: {}", e))),
            Err(_) => Err(self.err(name, format!("vsock rpc timed out after {:?}", timeout))),
        }
    }

    fn base_env(&self, name: &str) -> HashMap<String, String> {
        std::fs::read(self.config.vm_dir(name).join("env.json"))
            .ok()
            .and_then(|data| serde_json::from_slice(&data).ok())
            .unwrap_or_default()
    }

    /// Digests currently backing a VM dir → the sandbox names that reference
    /// them. A cache entry whose digest is absent here is reclaimable.
    fn referenced_digests(&self) -> HashMap<String, Vec<String>> {
        let mut refs: HashMap<String, Vec<String>> = HashMap::new();
        let Ok(entries) = std::fs::read_dir(self.config.vms_dir()) else {
            return refs;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Ok(digest) = std::fs::read_to_string(entry.path().join("image.digest")) {
                refs.entry(digest.trim().to_string())
                    .or_default()
                    .push(name);
            }
        }
        refs
    }

    /// Grow an offline ext4 image file to `target_bytes`: extend the file
    /// (sparse), then `resize2fs` to expand the filesystem into it. A forced
    /// `e2fsck -fy` first satisfies resize2fs's clean-fs precondition.
    async fn grow_rootfs(&self, img: &Path, target_bytes: u64) -> Result<(), AgentError> {
        let name = "-";
        let f = std::fs::OpenOptions::new().write(true).open(img)?;
        f.set_len(target_bytes)?;
        drop(f);
        // e2fsck return codes 0/1 (clean / errors-fixed) are both fine. Both
        // tools read a filesystem the guest wrote, so they run confined.
        let fsck = self.run_ext4_tool(name, img, "e2fsck", &["-fy"]).await?;
        if fsck.status.code().unwrap_or(8) > 1 {
            let said = String::from_utf8_lossy(&fsck.stderr);
            return Err(self.err(name, format!("e2fsck: {}{}", said, bubblewrap_hint(&said))));
        }
        let out = self.run_ext4_tool(name, img, "resize2fs", &[]).await?;
        if !out.status.success() {
            let said = String::from_utf8_lossy(&out.stderr);
            return Err(self.err(
                name,
                format!("resize2fs: {}{}", said, bubblewrap_hint(&said)),
            ));
        }
        Ok(())
    }

    async fn finish_create(
        &self,
        config: SandboxCreateConfig,
        name: String,
        image: String,
        vm_rootfs: PathBuf,
    ) -> Result<SandboxHandle, AgentError> {
        let cleanup_name = name.clone();
        let result = self
            .finish_create_inner(config, name, image, vm_rootfs)
            .await;
        if result.is_err() {
            if let Some(pid) = self.vm_pid(&cleanup_name) {
                // SAFETY: `kill` does not dereference memory. The PID comes
                // from this VM's pidfile and SIGKILL is used only while
                // rolling back a failed create, before its directory is removed.
                unsafe { libc::kill(pid as i32, libc::SIGKILL) };
            }
            self.release_tap(&cleanup_name).await;
            let _ = std::fs::remove_dir_all(self.config.vm_dir(&cleanup_name));
        }
        result
    }

    async fn finish_create_inner(
        &self,
        config: SandboxCreateConfig,
        name: String,
        image: String,
        vm_rootfs: PathBuf,
    ) -> Result<SandboxHandle, AgentError> {
        // Grow the per-VM disk to the requested size. Snapshot restores only
        // grow: a caller cannot truncate state captured in a larger disk.
        let base_bytes = std::fs::metadata(&vm_rootfs)?.len();
        let requested_bytes = config
            .disk_size_mb
            .unwrap_or(self.config.default_disk_mb)
            .saturating_mul(1024 * 1024);
        if requested_bytes > base_bytes {
            if let Err(e) = self.grow_rootfs(&vm_rootfs, requested_bytes).await {
                let _ = std::fs::remove_dir_all(self.config.vm_dir(&name));
                return Err(AgentError::SandboxCreationFailed {
                    run_id: config.run_id,
                    provider: "firecracker".to_string(),
                    reason: e.to_string(),
                });
            }
        }

        let vcpus = config
            .cpu_limit
            .map(|c| (c.ceil() as u32).max(1))
            .unwrap_or(self.config.default_vcpus);
        let mem = config
            .memory_limit_mb
            .unwrap_or(self.config.default_memory_mib);

        let networked = !matches!(
            config.network_mode.as_deref(),
            Some("none") | Some("restricted")
        );
        if config.network_mode.as_deref() == Some("restricted") {
            tracing::warn!(
                "firecracker: network_mode \"restricted\" not yet supported \
                 (needs the ADR-013 egress proxy); booting with no network"
            );
        }
        let mut boot_args =
            "console=ttyS0 reboot=k panic=1 pci=off init=/sbin/temps-vm-agent".to_string();
        let mut network_interfaces = Vec::new();
        if networked {
            let net = self.net_state().filter(|n| n.tap_count > 0);
            match net {
                Some(net) => {
                    let idx = self.allocate_tap(&name, &net).await.map_err(|e| {
                        AgentError::SandboxCreationFailed {
                            run_id: config.run_id,
                            provider: "firecracker".to_string(),
                            reason: e.to_string(),
                        }
                    })?;
                    let ip = net.guest_ip(idx);
                    let [a, b, c, d] = ip.octets();
                    boot_args.push_str(&format!(
                        " ip={}::{}:{}::eth0:off",
                        ip,
                        net.gateway,
                        net.netmask()
                    ));
                    network_interfaces.push(serde_json::json!({
                        "iface_id": "eth0",
                        "guest_mac": format!("06:fc:{:02x}:{:02x}:{:02x}:{:02x}", a, b, c, d),
                        "host_dev_name": format!("temps-fc-tap{}", idx),
                    }));
                }
                None => {
                    return Err(AgentError::SandboxCreationFailed {
                        run_id: config.run_id,
                        provider: "firecracker".to_string(),
                        reason: "sandbox requested network access but the host network \
                                 stage has not run — run `sudo temps firecracker setup \
                                 --network-only`, or create with network_mode \"none\""
                            .to_string(),
                    });
                }
            }
        }

        let vm_config = serde_json::json!({
            "boot-source": {
                "kernel_image_path": self.kernel_path()?,
                "boot_args": boot_args,
            },
            "drives": [{
                "drive_id": "rootfs",
                "path_on_host": "rootfs.ext4",
                "is_root_device": true,
                "is_read_only": false,
            }],
            "machine-config": { "vcpu_count": vcpus, "mem_size_mib": mem },
            "vsock": { "guest_cid": 3, "uds_path": "v.sock" },
            "network-interfaces": network_interfaces,
        });
        let vm_dir = self.config.vm_dir(&name);
        std::fs::write(
            vm_dir.join("vm.json"),
            serde_json::to_vec_pretty(&vm_config).map_err(|e| self.err(&name, e))?,
        )?;
        let env_path = vm_dir.join("env.json");
        std::fs::write(
            &env_path,
            serde_json::to_vec(&config.env_vars).map_err(|e| self.err(&name, e))?,
        )?;
        set_file_private(&env_path);

        if let Err(e) = self.spawn_vm(&name).await {
            let _ = std::fs::remove_dir_all(&vm_dir);
            self.release_tap(&name).await;
            return Err(AgentError::SandboxCreationFailed {
                run_id: config.run_id,
                provider: "firecracker".to_string(),
                reason: e.to_string(),
            });
        }

        tracing::info!("firecracker sandbox {} up (image {})", name, image);
        Ok(self.handle_with_image(&name, image))
    }

    async fn copy_sparse_file(
        &self,
        source: &Path,
        destination: &Path,
        sandbox_name: &str,
    ) -> Result<(), AgentError> {
        let copy = tokio::process::Command::new("cp")
            .args(["--sparse=always", "--reflink=auto"])
            .arg(source)
            .arg(destination)
            .output()
            .await;
        if copy.as_ref().is_ok_and(|output| output.status.success()) {
            return Ok(());
        }
        std::fs::copy(source, destination).map_err(|error| {
            let optimized_copy_error = match &copy {
                Ok(output) => String::from_utf8_lossy(&output.stderr).trim().to_string(),
                Err(command_error) => command_error.to_string(),
            };
            self.err(
                sandbox_name,
                format!(
                    "copy rootfs '{}' to '{}': optimized sparse copy failed ({}); fallback copy failed: {}",
                    source.display(),
                    destination.display(),
                    optimized_copy_error,
                    error
                ),
            )
        })?;
        Ok(())
    }

    fn hash_file(path: &Path) -> std::io::Result<String> {
        use sha2::{Digest, Sha256};
        use std::io::Read;

        let mut file = std::fs::File::open(path)?;
        let mut hasher = Sha256::new();
        let mut buffer = vec![0u8; 1024 * 1024];
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        Ok(hex::encode(hasher.finalize()))
    }

    fn scrub_snapshot_tree(root: &Path) -> std::io::Result<()> {
        use std::os::unix::fs::PermissionsExt;

        // Firecracker commands currently execute as root, so AI CLIs and
        // credential helpers can leave injected secrets under /root. Runtime
        // and platform credential state is ephemeral and must never enter a
        // reusable snapshot. The workspace remains untouched.
        for relative in ["root", "etc/temps", "tmp", "run", "var/tmp"] {
            let path = root.join(relative);
            match std::fs::remove_dir_all(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            std::fs::create_dir_all(&path)?;
        }
        std::fs::set_permissions(root.join("root"), std::fs::Permissions::from_mode(0o700))?;
        for relative in ["tmp", "run", "var/tmp"] {
            std::fs::set_permissions(root.join(relative), std::fs::Permissions::from_mode(0o1777))?;
        }
        Ok(())
    }

    fn path_with_suffix(path: &Path, suffix: &str) -> PathBuf {
        let mut suffixed = path.as_os_str().to_os_string();
        suffixed.push(suffix);
        PathBuf::from(suffixed)
    }

    fn snapshot_staging_exhausted(path: &Path) -> std::io::Result<bool> {
        use std::os::unix::ffi::OsStrExt;

        let path = std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "snapshot staging path contains a NUL byte",
            )
        })?;
        // SAFETY: `stats` points to writable initialized storage, `path` is a
        // valid NUL-terminated pathname, and `statvfs` retains neither pointer.
        let mut stats = unsafe { std::mem::zeroed::<libc::statvfs>() };
        let result = unsafe { libc::statvfs(path.as_ptr(), &mut stats) };
        if result != 0 {
            return Err(std::io::Error::last_os_error());
        }
        let available_bytes = (stats.f_bavail as u64).saturating_mul(stats.f_frsize as u64);
        Ok(available_bytes < 1024 * 1024 || stats.f_favail == 0)
    }

    async fn remove_stale_snapshot_path(path: &Path) -> std::io::Result<()> {
        let metadata = match tokio::fs::symlink_metadata(path).await {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        if metadata.is_dir() {
            tokio::fs::remove_dir_all(path).await
        } else {
            tokio::fs::remove_file(path).await
        }
    }

    async fn prepare_bounded_snapshot_staging(
        &self,
        destination: &Path,
        max_size_bytes: u64,
        sandbox_name: &str,
    ) -> Result<FuseSnapshotStaging, AgentError> {
        if max_size_bytes < MIN_SNAPSHOT_STAGING_BYTES {
            return Err(AgentError::SnapshotSizeLimitExceeded {
                sandbox_id: sandbox_name.to_string(),
                stage: format!(
                    "creating the minimum {} byte Firecracker extraction filesystem",
                    MIN_SNAPSHOT_STAGING_BYTES
                ),
                max_size_bytes,
            });
        }

        let mount_point = Self::path_with_suffix(destination, ".staging");
        let backing_file = Self::path_with_suffix(destination, ".extractor.ext4");

        // Recover from a process that exited while the private FUSE scratch
        // filesystem was mounted. `fusermount3` is the narrow setuid helper;
        // the Temps daemon itself never needs root or CAP_SYS_ADMIN.
        if FuseSnapshotStaging::path_requires_unmount(&mount_point).map_err(|error| {
            self.err(
                sandbox_name,
                format!(
                    "inspect stale snapshot staging mount '{}': {}",
                    mount_point.display(),
                    error
                ),
            )
        })? {
            FuseSnapshotStaging::unmount(&mount_point, true)
                .await
                .map_err(|error| {
                    self.err(
                        sandbox_name,
                        format!(
                            "detach stale snapshot staging mount '{}': {}",
                            mount_point.display(),
                            error
                        ),
                    )
                })?;
        }
        Self::remove_stale_snapshot_path(&mount_point)
            .await
            .map_err(|error| {
                self.err(
                    sandbox_name,
                    format!(
                        "remove stale snapshot staging path '{}': {}",
                        mount_point.display(),
                        error
                    ),
                )
            })?;
        Self::remove_stale_snapshot_path(&backing_file)
            .await
            .map_err(|error| {
                self.err(
                    sandbox_name,
                    format!(
                        "remove stale snapshot staging image '{}': {}",
                        backing_file.display(),
                        error
                    ),
                )
            })?;

        tokio::fs::create_dir_all(&mount_point)
            .await
            .map_err(|error| {
                self.err(
                    sandbox_name,
                    format!(
                        "create snapshot staging mount point '{}': {}",
                        mount_point.display(),
                        error
                    ),
                )
            })?;
        set_dir_private(&mount_point);

        let backing = tokio::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&backing_file)
            .await
            .map_err(|error| {
                self.err(
                    sandbox_name,
                    format!(
                        "create bounded snapshot staging image '{}': {}",
                        backing_file.display(),
                        error
                    ),
                )
            })?;
        if let Err(error) = backing.set_len(max_size_bytes).await {
            let _ = tokio::fs::remove_file(&backing_file).await;
            let _ = tokio::fs::remove_dir(&mount_point).await;
            return Err(self.err(
                sandbox_name,
                format!(
                    "size bounded snapshot staging image '{}' to {} bytes: {}",
                    backing_file.display(),
                    max_size_bytes,
                    error
                ),
            ));
        }
        drop(backing);
        set_file_private(&backing_file);

        let mkfs = tokio::process::Command::new("mkfs.ext4")
            .args(["-q", "-F", "-m", "0", "-O", "^has_journal"])
            .arg(&backing_file)
            .output()
            .await;
        let mkfs = match mkfs {
            Ok(output) if output.status.success() => output,
            Ok(output) => {
                let _ = tokio::fs::remove_file(&backing_file).await;
                let _ = tokio::fs::remove_dir(&mount_point).await;
                return Err(self.err(
                    sandbox_name,
                    format!(
                        "format bounded snapshot staging image '{}': {}",
                        backing_file.display(),
                        String::from_utf8_lossy(&output.stderr).trim()
                    ),
                ));
            }
            Err(error) => {
                let _ = tokio::fs::remove_file(&backing_file).await;
                let _ = tokio::fs::remove_dir(&mount_point).await;
                return Err(self.err(
                    sandbox_name,
                    format!(
                        "run mkfs.ext4 for bounded snapshot staging image '{}': {}",
                        backing_file.display(),
                        error
                    ),
                ));
            }
        };
        drop(mkfs);

        let mounted = tokio::process::Command::new("fuse2fs")
            .args(["-o", "rw,fakeroot,nosuid,nodev,noexec"])
            .arg(&backing_file)
            .arg(&mount_point)
            .output()
            .await;
        match mounted {
            Ok(output) if output.status.success() => {
                match FuseSnapshotStaging::path_is_mountpoint(&mount_point) {
                    Ok(true) => Ok(FuseSnapshotStaging {
                        mount_point,
                        backing_file,
                        mounted: true,
                    }),
                    Ok(false) => {
                        let _ = tokio::fs::remove_file(&backing_file).await;
                        let _ = tokio::fs::remove_dir(&mount_point).await;
                        Err(self.err(
                            sandbox_name,
                            format!(
                                "fuse2fs reported success but '{}' is not mounted",
                                mount_point.display()
                            ),
                        ))
                    }
                    Err(error) => {
                        let _ = FuseSnapshotStaging::unmount(&mount_point, true).await;
                        let _ = tokio::fs::remove_file(&backing_file).await;
                        let _ = tokio::fs::remove_dir(&mount_point).await;
                        Err(self.err(
                            sandbox_name,
                            format!(
                                "inspect bounded snapshot staging mount '{}': {}",
                                mount_point.display(),
                                error
                            ),
                        ))
                    }
                }
            }
            Ok(output) => {
                let _ = FuseSnapshotStaging::unmount(&mount_point, true).await;
                let _ = tokio::fs::remove_file(&backing_file).await;
                let _ = tokio::fs::remove_dir(&mount_point).await;
                Err(self.err(
                    sandbox_name,
                    format!(
                        "mount bounded snapshot staging image '{}' at '{}' with fuse2fs: {}",
                        backing_file.display(),
                        mount_point.display(),
                        String::from_utf8_lossy(&output.stderr).trim()
                    ),
                ))
            }
            Err(error) => {
                let _ = FuseSnapshotStaging::unmount(&mount_point, true).await;
                let _ = tokio::fs::remove_file(&backing_file).await;
                let _ = tokio::fs::remove_dir(&mount_point).await;
                Err(self.err(
                    sandbox_name,
                    format!(
                        "run fuse2fs for bounded snapshot staging image '{}' at '{}': {}",
                        backing_file.display(),
                        mount_point.display(),
                        error
                    ),
                ))
            }
        }
    }

    async fn run_snapshot_extractor(
        command: &mut tokio::process::Command,
        timeout: Duration,
    ) -> Result<SnapshotExtractorOutput, SnapshotExtractorError> {
        command
            .kill_on_drop(true)
            .stderr(std::process::Stdio::piped());
        let mut child = command.spawn().map_err(SnapshotExtractorError::Start)?;
        // Drain stderr concurrently so a chatty extractor cannot block on a
        // full pipe, keeping only the first bytes: the cause of a failure is
        // reported before any follow-on noise.
        let stderr_task = child.stderr.take().map(|mut stderr| {
            tokio::spawn(async move {
                let mut kept = Vec::with_capacity(SNAPSHOT_EXTRACTOR_STDERR_BYTES);
                let mut chunk = [0u8; 1024];
                loop {
                    match stderr.read(&mut chunk).await {
                        Ok(0) | Err(_) => break,
                        Ok(read) => {
                            let room = SNAPSHOT_EXTRACTOR_STDERR_BYTES - kept.len();
                            kept.extend_from_slice(&chunk[..read.min(room)]);
                        }
                    }
                }
                String::from_utf8_lossy(&kept).trim().to_string()
            })
        });
        match tokio::time::timeout(timeout, child.wait()).await {
            Ok(Ok(status)) => {
                let stderr = match stderr_task {
                    Some(task) => task.await.unwrap_or_default(),
                    None => String::new(),
                };
                Ok(SnapshotExtractorOutput { status, stderr })
            }
            Ok(Err(error)) => Err(SnapshotExtractorError::Wait(error)),
            Err(_) => {
                // `kill` waits for the child after sending SIGKILL. The
                // kill-on-drop fallback still prevents a surviving extractor
                // if the explicit kill itself reports an error.
                let _ = child.kill().await;
                Err(SnapshotExtractorError::TimedOut {
                    timeout_seconds: timeout.as_secs(),
                })
            }
        }
    }

    /// Run the child without ambient capabilities.
    ///
    /// Unprivileged bubblewrap exits 1 with "Unexpected capabilities but not
    /// setuid, old file caps config?" when it starts with any capability set.
    /// A server started by systemd with `AmbientCapabilities=` (the standard
    /// way to let a non-root `temps serve` bind port 80/443) passes that
    /// capability to every child, so the snapshot export failed on exactly the
    /// hosts that follow the install guide. The extractor needs no capability,
    /// so the ambient set is cleared in the child before `exec`; the server
    /// itself keeps its own.
    #[cfg(target_os = "linux")]
    fn without_ambient_capabilities(command: &mut tokio::process::Command) {
        // SAFETY: the closure runs between fork and exec and calls only
        // `prctl`, which is async-signal-safe. A kernel without ambient
        // capabilities (< 4.3) answers EINVAL, which is ignored: such a
        // child has none to clear.
        unsafe {
            command.pre_exec(|| {
                libc::prctl(
                    libc::PR_CAP_AMBIENT,
                    libc::PR_CAP_AMBIENT_CLEAR_ALL as libc::c_ulong,
                    0,
                    0,
                    0,
                );
                Ok(())
            });
        }
    }

    #[cfg(not(target_os = "linux"))]
    fn without_ambient_capabilities(_command: &mut tokio::process::Command) {}

    /// A `bwrap` with an empty, read-only root and no network, no `/proc`, no
    /// host `/dev`, no environment and no capabilities, to which a caller adds
    /// the one or two paths a tool needs. For parsing a filesystem image a
    /// guest wrote: that is hostile input to a host-side C parser, and the
    /// tool must not be able to reach anything else on the host.
    fn bwrap_isolated_base() -> tokio::process::Command {
        let mut command = tokio::process::Command::new("bwrap");
        Self::without_ambient_capabilities(&mut command);
        command.args([
            "--unshare-all",
            "--die-with-parent",
            "--new-session",
            "--cap-drop",
            "ALL",
            "--clearenv",
            "--setenv",
            "PATH",
            "/usr/sbin:/usr/bin:/sbin:/bin",
            "--ro-bind",
            "/usr",
            "/usr",
            "--ro-bind-try",
            "/bin",
            "/bin",
            "--ro-bind-try",
            "/sbin",
            "/sbin",
            "--ro-bind-try",
            "/lib",
            "/lib",
            "--ro-bind-try",
            "/lib64",
            "/lib64",
            "--dir",
            "/etc",
            "--dir",
            "/home",
            "--dir",
            "/root",
            "--dir",
            "/dev",
            "--dir",
            "/run",
            "--dir",
            "/tmp",
            "--remount-ro",
            "/",
        ]);
        command
    }

    fn sandboxed_debugfs_command(staging: &Path, source: &Path) -> tokio::process::Command {
        let mut command = Self::bwrap_isolated_base();
        command
            .arg("--bind")
            .arg(staging)
            .arg("/tmp")
            .arg("--ro-bind")
            .arg(source)
            .arg("/tmp/source.ext4")
            .args([
                "--chdir",
                "/tmp",
                "debugfs",
                "-R",
                "rdump / extracted",
                "/tmp/source.ext4",
            ]);
        command
    }

    /// `program args... <image>` inside the same confinement, with only
    /// `image` visible (writable) and nothing else of the host's data.
    fn sandboxed_ext4_tool_command(
        image: &Path,
        program: &str,
        args: &[&str],
    ) -> tokio::process::Command {
        let mut command = Self::bwrap_isolated_base();
        command
            .arg("--bind")
            .arg(image)
            .arg("/tmp/rootfs.ext4")
            .args(["--chdir", "/tmp", program])
            .args(args)
            .arg("/tmp/rootfs.ext4");
        command
    }

    /// Run a filesystem tool on a guest image to completion, bounded in time,
    /// killing it if the caller goes away.
    async fn run_ext4_tool(
        &self,
        name: &str,
        image: &Path,
        program: &'static str,
        args: &[&str],
    ) -> Result<std::process::Output, AgentError> {
        let mut command = Self::sandboxed_ext4_tool_command(image, program, args);
        command.kill_on_drop(true);
        match tokio::time::timeout(EXT4_TOOL_TIMEOUT, command.output()).await {
            Ok(Ok(output)) => Ok(output),
            Ok(Err(error)) => Err(self.err(
                name,
                format!(
                    "start {program} for '{}' inside bubblewrap (is bubblewrap installed? \
                     `temps firecracker setup` installs it): {error}",
                    image.display()
                ),
            )),
            Err(_) => Err(self.err(
                name,
                format!(
                    "{program} on '{}' exceeded its {} second timeout",
                    image.display(),
                    EXT4_TOOL_TIMEOUT.as_secs()
                ),
            )),
        }
    }

    async fn export_sanitized_rootfs(
        &self,
        source: &Path,
        destination: &Path,
        max_size_bytes: u64,
        sandbox_name: &str,
    ) -> Result<u64, AgentError> {
        let allocated_source_bytes = Self::try_disk_bytes(source).map_err(|error| {
            self.err(
                sandbox_name,
                format!(
                    "read allocated size for snapshot source '{}': {}",
                    source.display(),
                    error
                ),
            )
        })?;
        if allocated_source_bytes > max_size_bytes {
            return Err(AgentError::SnapshotSizeLimitExceeded {
                sandbox_id: sandbox_name.to_string(),
                stage: format!(
                    "checking the Firecracker rootfs ({} allocated bytes)",
                    allocated_source_bytes
                ),
                max_size_bytes,
            });
        }

        let staging_guard = self
            .prepare_bounded_snapshot_staging(destination, max_size_bytes, sandbox_name)
            .await?;
        let staging = staging_guard.mount_point.clone();
        let extracted = staging.join("extracted");

        let export_result: Result<u64, AgentError> = async {
            tokio::fs::create_dir_all(&extracted)
                .await
                .map_err(|error| {
                    self.err(
                        sandbox_name,
                        format!(
                            "create snapshot staging tree '{}': {}",
                            extracted.display(),
                            error
                        ),
                    )
                })?;

            // debugfs reads the stopped ext4 filesystem without mounting it
            // and materializes only live directory entries. Its `rdump`
            // command must never run directly on an untrusted guest image:
            // crafted ext dirent names containing `../` can otherwise escape
            // the output directory (e2fsprogs#272). Bubblewrap supplies the
            // parser boundary, while the staging bind is an unprivileged FUSE
            // mount of a private ext4 image whose total capacity is
            // max_size_bytes. Sparse guest inodes and hard-link amplification
            // therefore hit ENOSPC without consuming more host storage than
            // the snapshot quota or granting the daemon CAP_SYS_ADMIN.
            let mut dump_command = Self::sandboxed_debugfs_command(&staging, source);
            dump_command
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null());
            let dump = Self::run_snapshot_extractor(&mut dump_command, SNAPSHOT_EXTRACTION_TIMEOUT)
                .await
                .map_err(|error| {
                    self.err(
                        sandbox_name,
                        format!(
                            "run sandboxed debugfs for sanitized snapshot export: {}",
                            error
                        ),
                    )
                })?;

            let staging_for_capacity = staging.clone();
            let exhausted = tokio::task::spawn_blocking(move || {
                Self::snapshot_staging_exhausted(&staging_for_capacity)
            })
            .await
            .map_err(|error| {
                self.err(
                    sandbox_name,
                    format!("snapshot staging capacity task failed: {}", error),
                )
            })?
            .map_err(|error| {
                self.err(
                    sandbox_name,
                    format!(
                        "read bounded snapshot staging capacity '{}': {}",
                        staging.display(),
                        error
                    ),
                )
            })?;
            if exhausted {
                return Err(AgentError::SnapshotSizeLimitExceeded {
                    sandbox_id: sandbox_name.to_string(),
                    stage: "extracting Firecracker files into the bounded staging filesystem"
                        .to_string(),
                    max_size_bytes,
                });
            }
            if !dump.status.success() {
                return Err(self.err(
                    sandbox_name,
                    snapshot_export_failure_message(dump.status, &dump.stderr),
                ));
            }

            let extracted_for_task = extracted.clone();
            let content_bytes = tokio::task::spawn_blocking(move || -> std::io::Result<u64> {
                Self::scrub_snapshot_tree(&extracted_for_task)?;
                Ok(dir_size(&extracted_for_task))
            })
            .await
            .map_err(|error| {
                self.err(
                    sandbox_name,
                    format!("snapshot scrub task failed: {}", error),
                )
            })?
            .map_err(|error| {
                self.err(
                    sandbox_name,
                    format!(
                        "scrub snapshot staging tree '{}': {}",
                        extracted.display(),
                        error
                    ),
                )
            })?;

            if content_bytes > max_size_bytes {
                return Err(AgentError::SnapshotSizeLimitExceeded {
                    sandbox_id: sandbox_name.to_string(),
                    stage: format!(
                        "exporting {} bytes of live Firecracker files",
                        content_bytes
                    ),
                    max_size_bytes,
                });
            }

            let filesystem_bytes = content_bytes
                .saturating_mul(3)
                .saturating_div(2)
                .saturating_add(CACHE_SLACK_MB * 1024 * 1024)
                .next_multiple_of(4096);
            if filesystem_bytes > max_size_bytes {
                return Err(AgentError::SnapshotSizeLimitExceeded {
                    sandbox_id: sandbox_name.to_string(),
                    stage: format!(
                        "sizing the sanitized Firecracker filesystem at {} bytes",
                        filesystem_bytes
                    ),
                    max_size_bytes,
                });
            }

            let blocks = filesystem_bytes / 4096;
            let mkfs = tokio::process::Command::new("mkfs.ext4")
                .args([
                    "-q",
                    "-F",
                    "-b",
                    "4096",
                    "-E",
                    "lazy_itable_init=0,lazy_journal_init=0",
                    "-d",
                ])
                .arg(&extracted)
                .arg(destination)
                .arg(blocks.to_string())
                .output()
                .await
                .map_err(|error| {
                    self.err(
                        sandbox_name,
                        format!("run mkfs.ext4 for sanitized snapshot: {}", error),
                    )
                })?;
            if !mkfs.status.success() {
                return Err(self.err(
                    sandbox_name,
                    format!(
                        "mkfs.ext4 sanitized snapshot failed: {}",
                        String::from_utf8_lossy(&mkfs.stderr).trim()
                    ),
                ));
            }

            let artifact_bytes = Self::try_disk_bytes(destination).map_err(|error| {
                self.err(
                    sandbox_name,
                    format!(
                        "read allocated size for snapshot artifact '{}': {}",
                        destination.display(),
                        error
                    ),
                )
            })?;
            if artifact_bytes > max_size_bytes {
                return Err(AgentError::SnapshotSizeLimitExceeded {
                    sandbox_id: sandbox_name.to_string(),
                    stage: format!(
                        "publishing the {} byte Firecracker artifact",
                        artifact_bytes
                    ),
                    max_size_bytes,
                });
            }
            Ok(artifact_bytes)
        }
        .await;

        let cleanup_result = staging_guard.cleanup().await;
        if let Err(error) = cleanup_result {
            let _ = tokio::fs::remove_file(destination).await;
            return Err(self.err(
                sandbox_name,
                format!(
                    "clean bounded Firecracker snapshot staging filesystem '{}': {}",
                    staging.display(),
                    error
                ),
            ));
        }
        if export_result.is_err() {
            let _ = tokio::fs::remove_file(destination).await;
        }
        export_result
    }

    /// Actual on-disk bytes for a file (sparse-aware: counts allocated
    /// blocks, not the apparent length).
    fn try_disk_bytes(path: &Path) -> std::io::Result<u64> {
        use std::os::unix::fs::MetadataExt;
        Ok(std::fs::metadata(path)?.blocks() * 512)
    }

    /// Best-effort allocated size for status and cleanup accounting.
    fn disk_bytes(path: &Path) -> u64 {
        Self::try_disk_bytes(path).unwrap_or(0)
    }
}

#[async_trait]
impl SandboxProvider for FirecrackerSandboxProvider {
    async fn create(&self, config: SandboxCreateConfig) -> Result<SandboxHandle, AgentError> {
        config.validate_allow_docker(super::SandboxBackend::Firecracker)?;
        config.refuse_restricted_egress(super::SandboxBackend::Firecracker)?;
        let name = self.resolve_name(&config);
        let vm_dir = self.config.vm_dir(&name);
        let image = config
            .image
            .clone()
            .filter(|i| !i.is_empty())
            .unwrap_or_else(|| DEFAULT_IMAGE.to_string());

        // Convert (or reuse) the cached rootfs, copy it for this VM, and mark
        // the cache entry as referenced — ALL under the cache lock so a
        // concurrent destroy-triggered GC can't delete the entry between the
        // conversion and the reference marker landing. Only the fast bits are
        // in the critical section; the VM boot below is not.
        let vm_rootfs = vm_dir.join("rootfs.ext4");
        {
            let _cache_guard = self.cache_lock.lock().await;
            let rootfs_cache = self.ensure_rootfs_locked(&image).await.map_err(|e| {
                AgentError::SandboxCreationFailed {
                    run_id: config.run_id,
                    provider: "firecracker".to_string(),
                    reason: e.to_string(),
                }
            })?;

            std::fs::create_dir_all(&vm_dir)?;
            // The VM dir holds the rootfs, sockets, and `env.json` (injected
            // credentials like ANTHROPIC_API_KEY). 0700 keeps other local
            // users out of the secrets.
            set_dir_private(&vm_dir);
            // Per-VM writable copy. `cp --sparse=always` (SEEK_HOLE-aware)
            // reproduces the source's holes — `std::fs::copy`'s
            // copy_file_range path doesn't reliably preserve sparseness on
            // ext4 and inflates each per-VM disk to the full nominal size.
            let cp = tokio::process::Command::new("cp")
                .args(["--sparse=always", "--reflink=auto"])
                .arg(&rootfs_cache)
                .arg(&vm_rootfs)
                .output()
                .await?;
            if !cp.status.success() {
                std::fs::copy(&rootfs_cache, &vm_rootfs)?;
            }
            // Reference marker (cache file stem is the digest). Written under
            // the lock so GC sees it before it can consider the entry orphaned.
            if let Some(digest) = rootfs_cache.file_stem().and_then(|s| s.to_str()) {
                let _ = std::fs::write(vm_dir.join("image.digest"), digest);
            }
        }

        self.finish_create(config, name, image, vm_rootfs).await
    }

    async fn exec(
        &self,
        handle: &SandboxHandle,
        cmd: Vec<String>,
        env: HashMap<String, String>,
        on_output: Option<OnEventCallback>,
    ) -> Result<SandboxExecResult, AgentError> {
        let mut merged = self.base_env(&handle.sandbox_name);
        merged.extend(env);
        let response = self
            .rpc(
                &handle.sandbox_name,
                &Request::Exec {
                    cmd,
                    env: merged,
                    cwd: Some(handle.work_dir.to_string_lossy().into_owned()),
                    user: None,
                    timeout_secs: None,
                },
                RPC_TIMEOUT,
            )
            .await?;
        match response {
            Response::Exec {
                exit_code,
                stdout,
                stderr,
            } => {
                // v1 delivers output post-hoc (no mid-run streaming yet) —
                // callback consumers still see every line.
                if let Some(cb) = on_output {
                    for line in stdout.lines() {
                        cb(line.to_string()).await;
                    }
                }
                Ok(SandboxExecResult {
                    exit_code,
                    stdout,
                    stderr,
                })
            }
            Response::Err { message } => Err(self.err(&handle.sandbox_name, message)),
            other => Err(self.err(
                &handle.sandbox_name,
                format!("unexpected agent response: {:?}", other),
            )),
        }
    }

    async fn is_alive(&self, handle: &SandboxHandle) -> Result<bool, AgentError> {
        let vmm_running = self.vm_pid(&handle.sandbox_name).is_some();
        let agent_answered = vmm_running
            && matches!(
                self.rpc(&handle.sandbox_name, &Request::Ping, Duration::from_secs(3))
                    .await,
                Ok(Response::Pong)
            );
        liveness_verdict(vmm_running, agent_answered)
            .map_err(|reason| self.err(&handle.sandbox_name, reason))
    }

    async fn write_file(
        &self,
        handle: &SandboxHandle,
        path: &str,
        contents: &[u8],
        mode: u32,
    ) -> Result<(), AgentError> {
        let response = self
            .rpc(
                &handle.sandbox_name,
                &Request::WriteFile {
                    path: path.to_string(),
                    data_hex: hex::encode(contents),
                    mode,
                },
                RPC_TIMEOUT,
            )
            .await?;
        match response {
            Response::Ok => Ok(()),
            Response::Err { message } => Err(self.err(&handle.sandbox_name, message)),
            other => Err(self.err(
                &handle.sandbox_name,
                format!("unexpected agent response: {:?}", other),
            )),
        }
    }

    async fn read_file(&self, handle: &SandboxHandle, path: &str) -> Result<Vec<u8>, AgentError> {
        let response = self
            .rpc(
                &handle.sandbox_name,
                &Request::ReadFile {
                    path: path.to_string(),
                },
                RPC_TIMEOUT,
            )
            .await?;
        match response {
            Response::File { data_hex } => hex::decode(&data_hex)
                .map_err(|e| self.err(&handle.sandbox_name, format!("bad hex from agent: {}", e))),
            Response::Err { message } => Err(self.err(&handle.sandbox_name, message)),
            other => Err(self.err(
                &handle.sandbox_name,
                format!("unexpected agent response: {:?}", other),
            )),
        }
    }

    async fn write_directory(
        &self,
        handle: &SandboxHandle,
        local_dir: &Path,
        target_path: &str,
    ) -> Result<(), AgentError> {
        // v1: per-file RPCs. Fine for seeding small trees; a tar-stream op
        // lands with the pty/vsock unification for big workdirs.
        let mut stack = vec![local_dir.to_path_buf()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir)? {
                let entry = entry?;
                let path = entry.path();
                let rel = path
                    .strip_prefix(local_dir)
                    .map_err(|e| self.err(&handle.sandbox_name, e))?;
                let target = format!("{}/{}", target_path.trim_end_matches('/'), rel.display());
                let meta = entry.metadata()?;
                if meta.is_dir() {
                    stack.push(path);
                } else if meta.is_file() {
                    use std::os::unix::fs::PermissionsExt;
                    let contents = std::fs::read(&path)?;
                    self.write_file(
                        handle,
                        &target,
                        &contents,
                        meta.permissions().mode() & 0o777,
                    )
                    .await?;
                }
            }
        }
        Ok(())
    }

    async fn kill_processes(
        &self,
        handle: &SandboxHandle,
        pattern: &str,
        signal: KillSignal,
    ) -> Result<(), AgentError> {
        let _ = self
            .rpc(
                &handle.sandbox_name,
                &Request::Kill {
                    pattern: pattern.to_string(),
                    signal: signal.as_number(),
                },
                Duration::from_secs(10),
            )
            .await?;
        Ok(())
    }

    async fn destroy(
        &self,
        handle: &SandboxHandle,
        _purge_volumes: bool,
    ) -> Result<(), AgentError> {
        let name = &handle.sandbox_name;
        let _ = self.stop(handle).await;
        if let Some(pid) = self.vm_pid(name) {
            unsafe { libc::kill(pid as i32, libc::SIGKILL) };
        }
        self.release_tap(name).await;
        let _ = std::fs::remove_dir_all(self.config.vm_dir(name));
        tracing::info!("firecracker sandbox {} destroyed", name);
        // Reclaim any cache entry this was the last VM to reference, so the
        // rootfs cache only ever holds what live sandboxes need.
        let _ = self.gc_rootfs().await;
        Ok(())
    }

    async fn stop(&self, handle: &SandboxHandle) -> Result<(), AgentError> {
        let name = &handle.sandbox_name;
        let Some(pid) = self.vm_pid(name) else {
            return Ok(()); // already stopped
        };
        // Graceful: agent syncs and powers off, VMM exits on guest reboot.
        let _ = self
            .rpc(name, &Request::Shutdown, Duration::from_secs(5))
            .await;
        let deadline = tokio::time::Instant::now() + SHUTDOWN_GRACE;
        while self.vm_pid(name).is_some() {
            if tokio::time::Instant::now() > deadline {
                unsafe { libc::kill(pid as i32, libc::SIGKILL) };
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let _ = std::fs::remove_file(self.config.vm_dir(name).join("fc.pid"));
        Ok(())
    }

    async fn start(&self, handle: &SandboxHandle) -> Result<(), AgentError> {
        let name = &handle.sandbox_name;
        if self.vm_pid(name).is_some() {
            return Ok(()); // already running
        }
        if !self.config.vm_dir(name).join("vm.json").exists() {
            return Err(self.err(name, "no persisted VM config; sandbox was destroyed"));
        }
        // Rootfs persisted across stop — the VM resumes its filesystem state.
        self.spawn_vm(name).await
    }

    async fn resize_disk(
        &self,
        handle: &SandboxHandle,
        new_size_mb: u64,
    ) -> Result<(), AgentError> {
        let name = &handle.sandbox_name;
        let vm_rootfs = self.config.vm_dir(name).join("rootfs.ext4");
        if !vm_rootfs.exists() {
            return Err(self.err(name, "sandbox has no rootfs to resize"));
        }
        let target = new_size_mb.saturating_mul(1024 * 1024);
        let current = std::fs::metadata(&vm_rootfs)?.len();
        if target <= current {
            return Err(self.err(
                name,
                format!(
                    "disk can only grow: current {} MiB, requested {} MiB",
                    current / 1024 / 1024,
                    new_size_mb
                ),
            ));
        }
        // Offline resize so it works for any guest image (no in-guest
        // resize2fs needed). Stop → grow the ext4 → restart. The filesystem
        // and its data survive the brief reboot.
        let was_running = self.vm_pid(name).is_some();
        if was_running {
            self.stop(handle).await?;
        }
        self.grow_rootfs(&vm_rootfs, target).await?;
        if was_running {
            self.start(handle).await?;
        }
        tracing::info!(
            "firecracker sandbox {} disk grown to {} MiB",
            name,
            new_size_mb
        );
        Ok(())
    }

    async fn recover(&self, run_id: i32) -> Result<Option<SandboxHandle>, AgentError> {
        self.recover_by_name(&format!("{}{}", FC_SANDBOX_NAME_PREFIX, run_id))
            .await
    }

    async fn recover_by_name(
        &self,
        container_name: &str,
    ) -> Result<Option<SandboxHandle>, AgentError> {
        // Accept both the full VM name and the bare label the standalone
        // registry passes (it only knows Docker's naming convention).
        let name = if container_name.starts_with(FC_SANDBOX_NAME_PREFIX) {
            container_name.to_string()
        } else {
            format!("{}{}", FC_SANDBOX_NAME_PREFIX, container_name)
        };
        if self.config.vm_dir(&name).join("vm.json").exists() {
            Ok(Some(self.handle_for(&name)))
        } else {
            Ok(None)
        }
    }

    async fn take_snapshot(
        &self,
        handle: &SandboxHandle,
        _label: Option<String>,
        max_size_bytes: u64,
    ) -> Result<super::SnapshotArtifact, AgentError> {
        let name = &handle.sandbox_name;
        if self.vm_pid(name).is_some() {
            return Err(self.err(
                name,
                "snapshot requires a stopped Firecracker VM for filesystem consistency",
            ));
        }

        let source = self.config.vm_dir(name).join("rootfs.ext4");
        if !source.exists() {
            return Err(self.err(
                name,
                format!("snapshot source rootfs '{}' is missing", source.display()),
            ));
        }

        let snapshots_dir = self.config.data_dir.join("snapshots");
        tokio::fs::create_dir_all(&snapshots_dir)
            .await
            .map_err(|error| {
                self.err(
                    name,
                    format!(
                        "create snapshot directory '{}': {}",
                        snapshots_dir.display(),
                        error
                    ),
                )
            })?;
        let temporary = snapshots_dir.join(format!(".tmp-firecracker-{}", name));
        let _ = tokio::fs::remove_file(&temporary).await;
        let size_bytes = self
            .export_sanitized_rootfs(&source, &temporary, max_size_bytes, name)
            .await?;

        let hash_path = temporary.clone();
        let hash_result = tokio::task::spawn_blocking(move || Self::hash_file(&hash_path)).await;
        let digest = match hash_result {
            Ok(Ok(digest)) => digest,
            Ok(Err(error)) => {
                let _ = tokio::fs::remove_file(&temporary).await;
                return Err(self.err(
                    name,
                    format!("hash snapshot rootfs '{}': {}", temporary.display(), error),
                ));
            }
            Err(error) => {
                let _ = tokio::fs::remove_file(&temporary).await;
                return Err(self.err(name, format!("snapshot hash task failed: {}", error)));
            }
        };
        let final_path = snapshots_dir.join(format!("{}.ext4", digest));
        match tokio::fs::hard_link(&temporary, &final_path).await {
            Ok(()) => {
                // The published hard link owns the inode now; remove only the
                // private staging name.
                let _ = tokio::fs::remove_file(&temporary).await;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                // Never replace a content-addressed artifact. Verify that an
                // existing file really contains the bytes its name promises
                // before sharing it with the new database row.
                let existing_path = final_path.clone();
                let existing_hash =
                    tokio::task::spawn_blocking(move || Self::hash_file(&existing_path)).await;
                let _ = tokio::fs::remove_file(&temporary).await;
                let existing_digest = existing_hash
                    .map_err(|task_error| {
                        self.err(
                            name,
                            format!("existing snapshot hash task failed: {}", task_error),
                        )
                    })?
                    .map_err(|hash_error| {
                        self.err(
                            name,
                            format!(
                                "hash existing Firecracker snapshot '{}': {}",
                                final_path.display(),
                                hash_error
                            ),
                        )
                    })?;
                if existing_digest != digest {
                    return Err(self.err(
                        name,
                        format!(
                            "existing Firecracker snapshot '{}' failed content-address verification: expected {}, got {}",
                            final_path.display(),
                            digest,
                            existing_digest
                        ),
                    ));
                }
            }
            Err(error) => {
                let _ = tokio::fs::remove_file(&temporary).await;
                return Err(self.err(
                    name,
                    format!(
                        "publish Firecracker snapshot '{}' as '{}': {}",
                        temporary.display(),
                        final_path.display(),
                        error
                    ),
                ));
            }
        }

        tracing::info!(
            sandbox = %name,
            digest = %digest,
            size_bytes,
            path = %final_path.display(),
            "firecracker snapshot completed"
        );
        Ok(super::SnapshotArtifact {
            content_path: final_path,
            content_digest: digest.clone(),
            primary_digest: digest,
            size_bytes,
            backend: super::SandboxBackend::Firecracker,
            image_ref: None,
            image_id: None,
            workspace: None,
        })
    }

    async fn create_from_snapshot(
        &self,
        artifact: &super::SnapshotArtifact,
        config: SandboxCreateConfig,
    ) -> Result<SandboxHandle, AgentError> {
        config.validate_allow_docker(super::SandboxBackend::Firecracker)?;
        config.refuse_restricted_egress(super::SandboxBackend::Firecracker)?;
        if artifact.backend != super::SandboxBackend::Firecracker {
            return Err(AgentError::SandboxCreationFailed {
                run_id: config.run_id,
                provider: "firecracker".to_string(),
                reason: format!(
                    "cannot restore '{}' snapshot artifact with Firecracker",
                    artifact.backend
                ),
            });
        }

        let source = artifact.content_path.clone();
        if artifact.content_digest != artifact.primary_digest {
            return Err(AgentError::SandboxCreationFailed {
                run_id: config.run_id,
                provider: "firecracker".to_string(),
                reason: format!(
                    "Firecracker snapshot metadata is inconsistent: logical digest {} differs from rootfs digest {}",
                    artifact.content_digest, artifact.primary_digest
                ),
            });
        }
        let expected_digest = artifact.primary_digest.clone();
        let hash_source = source.clone();
        let actual_digest = tokio::task::spawn_blocking(move || Self::hash_file(&hash_source))
            .await
            .map_err(|error| AgentError::SandboxCreationFailed {
                run_id: config.run_id,
                provider: "firecracker".to_string(),
                reason: format!("snapshot hash task failed: {}", error),
            })?
            .map_err(|error| AgentError::SandboxCreationFailed {
                run_id: config.run_id,
                provider: "firecracker".to_string(),
                reason: format!("hash snapshot rootfs '{}': {}", source.display(), error),
            })?;
        if actual_digest != expected_digest {
            return Err(AgentError::SandboxCreationFailed {
                run_id: config.run_id,
                provider: "firecracker".to_string(),
                reason: format!(
                    "snapshot rootfs digest mismatch: expected {}, got {}",
                    expected_digest, actual_digest
                ),
            });
        }

        let name = self.resolve_name(&config);
        let vm_dir = self.config.vm_dir(&name);
        std::fs::create_dir_all(&vm_dir)?;
        set_dir_private(&vm_dir);
        let vm_rootfs = vm_dir.join("rootfs.ext4");
        if let Err(error) = self.copy_sparse_file(&source, &vm_rootfs, &name).await {
            let _ = std::fs::remove_dir_all(&vm_dir);
            return Err(error);
        }

        let image = format!(
            "firecracker-snapshot:{}",
            &expected_digest[..expected_digest.len().min(12)]
        );
        self.finish_create(config, name, image, vm_rootfs).await
    }

    /// Bridge to the guest's `temps-pty-agent` over vsock port
    /// [`PTY_PORT`] (ADR-029 §5).
    ///
    /// `temps-vm-agent` runs the PTY agent and relays this connection to its
    /// unix socket, so no process is started per attach and the VM needs no
    /// `socat`. Dropping the returned `input` closes the vsock connection;
    /// the agent — and every tab — keeps running in the guest.
    async fn attach_pty(&self, handle: &SandboxHandle) -> Result<PtyAttachment, AgentError> {
        self.attach_pty_within(handle, PTY_ATTACH_TIMEOUT).await
    }

    fn supports_backend(&self, backend: super::SandboxBackend) -> bool {
        matches!(backend, super::SandboxBackend::Firecracker)
    }

    fn default_resource_limits(
        &self,
        _backend: Option<super::SandboxBackend>,
    ) -> super::DefaultResourceLimits {
        super::DefaultResourceLimits {
            cpu_limit: Some(f64::from(self.config.default_vcpus)),
            memory_limit_mb: Some(self.config.default_memory_mib),
            pids_limit: None,
            cpu_whole_cores: true,
        }
    }

    fn name(&self) -> &str {
        "firecracker"
    }

    async fn is_available(&self) -> bool {
        // Provisioned (setup ran, smoke passed) + still-true host facts.
        let state_ok = std::fs::read(self.config.fc_root().join("state.json"))
            .ok()
            .and_then(|data| serde_json::from_slice::<serde_json::Value>(&data).ok())
            .is_some_and(|s| s["smoke_ok"].as_bool().unwrap_or(false));
        state_ok
            && self.config.firecracker_bin().exists()
            && self.config.agent_bin().exists()
            && self.kernel_path().is_ok()
            && std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open("/dev/kvm")
                .is_ok()
            && firecracker_snapshot_host_available()
    }

    async fn image_status(&self) -> Result<(bool, String), AgentError> {
        // Rootfs conversion is lazy and per-image; the backend itself being
        // provisioned is the meaningful readiness signal here.
        Ok((self.is_available().await, DEFAULT_IMAGE.to_string()))
    }

    async fn rebuild_image(&self) -> Result<String, AgentError> {
        // Drop the conversion cache; next create reconverts from Docker.
        let _ = std::fs::remove_dir_all(self.config.cache_dir());
        Ok(DEFAULT_IMAGE.to_string())
    }

    async fn rootfs_report(&self) -> Result<super::RootfsReport, AgentError> {
        let refs = self.referenced_digests();

        // Cache entries, tagged with the sandboxes that reference them.
        let mut cache = Vec::new();
        let mut cache_bytes = 0u64;
        if let Ok(entries) = std::fs::read_dir(self.config.cache_dir()) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("ext4") {
                    continue;
                }
                let Some(digest) = path.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };
                let bytes = Self::disk_bytes(&path);
                cache_bytes += bytes;
                cache.push(super::RootfsCacheEntry {
                    digest: digest.to_string(),
                    bytes,
                    referenced_by: refs.get(digest).cloned().unwrap_or_default(),
                });
            }
        }

        // Per-VM disks — the authoritative rootfs storage.
        let mut vms = Vec::new();
        let mut vm_bytes = 0u64;
        if let Ok(entries) = std::fs::read_dir(self.config.vms_dir()) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                let bytes = Self::disk_bytes(&entry.path().join("rootfs.ext4"));
                vm_bytes += bytes;
                vms.push(super::RootfsVmEntry {
                    running: self.vm_pid(&name).is_some(),
                    sandbox_name: name,
                    bytes,
                });
            }
        }

        Ok(super::RootfsReport {
            cache_bytes,
            cache,
            vm_bytes,
            vms,
        })
    }

    async fn gc_rootfs(&self) -> Result<super::RootfsGcReport, AgentError> {
        // Hold the cache lock so we never race an in-flight `create` between
        // its conversion and its reference marker (see `ensure_rootfs_locked`).
        let _cache_guard = self.cache_lock.lock().await;
        let refs = self.referenced_digests();
        let mut report = super::RootfsGcReport::default();
        let Ok(entries) = std::fs::read_dir(self.config.cache_dir()) else {
            return Ok(report);
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("ext4") {
                continue;
            }
            let Some(digest) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            // Keep only entries that back an existing sandbox's VM disk.
            if refs.contains_key(digest) {
                continue;
            }
            let bytes = Self::disk_bytes(&path);
            if std::fs::remove_file(&path).is_ok() {
                report.freed_bytes += bytes;
                report.removed_digests.push(digest.to_string());
            }
        }
        if !report.removed_digests.is_empty() {
            tracing::info!(
                "firecracker rootfs GC: reclaimed {} cache entr{} ({} bytes)",
                report.removed_digests.len(),
                if report.removed_digests.len() == 1 {
                    "y"
                } else {
                    "ies"
                },
                report.freed_bytes
            );
        }
        Ok(report)
    }
}

/// Open a stream to a guest vsock `port` through Firecracker's hybrid-vsock
/// unix socket (`CONNECT <port>\n` → `OK <hostport>\n`).
///
/// The ack is read **one byte at a time from the unbuffered stream**. A
/// buffered reader could pull bytes that follow the ack into its own buffer
/// and lose them when the stream is handed on — harmless for the RPC (the
/// guest only speaks after the request), fatal for a terminal, where the
/// agent's first frame can arrive immediately behind the ack.
async fn vsock_connect(sock: &Path, port: u32) -> std::io::Result<UnixStream> {
    let mut stream = UnixStream::connect(sock).await?;
    stream
        .write_all(format!("CONNECT {}\n", port).as_bytes())
        .await?;
    let mut ack = Vec::with_capacity(32);
    loop {
        let b = stream.read_u8().await?;
        if b == b'\n' {
            break;
        }
        ack.push(b);
        if ack.len() > 64 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "vsock handshake overflow",
            ));
        }
    }
    if !ack.starts_with(b"OK") {
        return Err(std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            format!("vsock handshake: {}", String::from_utf8_lossy(&ack)),
        ));
    }
    Ok(stream)
}

/// Resolve `rel` inside `root` the way a chroot at `root` would, without
/// ever leaving it, and return the real path under `root`.
///
/// Image contents are untrusted: any component may be a symlink, absolute
/// targets included (`etc -> /etc`), and `..` may try to climb out. A plain
/// `root.join(rel)` followed by a filesystem call follows those links onto
/// the *host*. Here an absolute target restarts at `root`, `..` stops at
/// `root`, and the walk is bounded so a link cycle fails instead of looping.
/// Components that do not exist yet are kept as plain names.
///
/// The result is only symlink-free for the components that existed when it
/// was computed, which is sufficient because extraction has finished and the
/// tree is private to this process.
fn rootfs_resolve(root: &Path, rel: &Path) -> std::io::Result<PathBuf> {
    use std::collections::VecDeque;
    use std::ffi::OsString;
    use std::path::Component;

    const MAX_SYMLINK_HOPS: usize = 40;

    #[derive(Debug)]
    enum Part {
        Parent,
        Name(OsString),
    }

    fn parts_of(path: &Path) -> Vec<Part> {
        path.components()
            .filter_map(|c| match c {
                Component::ParentDir => Some(Part::Parent),
                Component::Normal(name) => Some(Part::Name(name.to_os_string())),
                // `/`, prefixes and `.` carry no name to resolve.
                Component::RootDir | Component::Prefix(_) | Component::CurDir => None,
            })
            .collect()
    }

    let mut pending: VecDeque<Part> = parts_of(rel).into();
    let mut resolved = PathBuf::new();
    let mut hops = 0usize;
    while let Some(part) = pending.pop_front() {
        match part {
            Part::Parent => {
                // At the root `..` is the root, as in a real chroot.
                resolved.pop();
            }
            Part::Name(name) => {
                let candidate = root.join(&resolved).join(&name);
                match std::fs::symlink_metadata(&candidate) {
                    Ok(meta) if meta.file_type().is_symlink() => {
                        hops += 1;
                        if hops > MAX_SYMLINK_HOPS {
                            return Err(std::io::Error::new(
                                std::io::ErrorKind::InvalidData,
                                format!(
                                    "too many symbolic links resolving {} inside the image rootfs",
                                    rel.display()
                                ),
                            ));
                        }
                        let target = std::fs::read_link(&candidate)?;
                        if target.is_absolute() {
                            resolved.clear();
                        }
                        for part in parts_of(&target).into_iter().rev() {
                            pending.push_front(part);
                        }
                    }
                    _ => resolved.push(name),
                }
            }
        }
    }
    Ok(root.join(resolved))
}

/// Remove `rel` from the image rootfs without following symlinks out of it.
/// A missing entry is not an error; a symlink at the final component is
/// removed itself, never its target.
fn rootfs_remove(root: &Path, rel: &Path) -> std::io::Result<()> {
    let Some(name) = rel.file_name() else {
        return Ok(());
    };
    let dir = rootfs_resolve(root, rel.parent().unwrap_or_else(|| Path::new("")))?;
    let path = dir.join(name);
    match std::fs::symlink_metadata(&path) {
        Ok(meta) if meta.is_dir() => std::fs::remove_dir_all(&path),
        Ok(_) => std::fs::remove_file(&path),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Write the host file `source` to `rel` inside the image rootfs, keeping its
/// mode. Whatever the image had at that path (a file, a directory, a symlink
/// to a host file) is replaced, never written through, and the new file is
/// created exclusively so a racing link cannot redirect the write.
fn rootfs_inject(root: &Path, rel: &Path, source: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;

    let name = rel.file_name().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("injection path {} has no file name", rel.display()),
        )
    })?;
    let dir = rootfs_resolve(root, rel.parent().unwrap_or_else(|| Path::new("")))?;
    std::fs::create_dir_all(&dir)?;
    let dest = dir.join(name);
    rootfs_remove(root, rel)?;

    let mut input = std::fs::File::open(source)?;
    let mode = {
        use std::os::unix::fs::PermissionsExt;
        input.metadata()?.permissions().mode() & 0o777
    };
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(&dest)?;
    std::io::copy(&mut input, &mut output)?;
    Ok(())
}

/// Restrict a directory to the owner (0700). Best-effort — a failure is
/// logged, not fatal, since it only weakens local isolation.
fn set_dir_private(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Err(e) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)) {
        tracing::warn!("failed to 0700 {}: {}", path.display(), e);
    }
}

/// Restrict a file to the owner (0600) — used for `env.json`, which carries
/// injected credentials.
fn set_file_private(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Err(e) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)) {
        tracing::warn!("failed to 0600 {}: {}", path.display(), e);
    }
}

/// Total apparent size of a directory tree (bytes), without following
/// guest-controlled symlinks outside the staging tree or into cycles.
fn dir_size(root: &Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(meta) = entry.path().symlink_metadata() else {
                continue;
            };
            if meta.is_dir() {
                stack.push(entry.path());
            } else {
                total = total.saturating_add(meta.len());
            }
        }
    }
    total
}

/// Check whether the Firecracker backend is ready on this host given the
/// Temps data directory. Mirrors the logic in `FirecrackerSandboxProvider::is_available`
/// without requiring a fully-constructed provider — used by the settings
/// health endpoint so the UI can report backend availability without
/// instantiating the full agent executor.
pub async fn is_firecracker_available(data_dir: &Path) -> bool {
    let config = FirecrackerSandboxConfig::from_data_dir(data_dir.to_path_buf());
    let state_ok = std::fs::read(config.fc_root().join("state.json"))
        .ok()
        .and_then(|data| serde_json::from_slice::<serde_json::Value>(&data).ok())
        .is_some_and(|s| s["smoke_ok"].as_bool().unwrap_or(false));
    state_ok
        && config.firecracker_bin().exists()
        && config.agent_bin().exists()
        && std::fs::read_dir(config.kernel_glob_dir())
            .ok()
            .and_then(|mut d| d.next())
            .is_some()
        && std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/kvm")
            .is_ok()
        && firecracker_snapshot_host_available()
}

/// Whether Firecracker sandboxes created on this host can open interactive
/// terminals: the static `temps-pty-agent` must be staged for injection.
/// Reported next to [`is_firecracker_available`] so the console can onboard
/// ("stage the terminal agent") instead of surfacing a bare 409 later.
pub fn firecracker_terminal_capability(data_dir: &Path) -> super::TerminalCapability {
    let config = FirecrackerSandboxConfig::from_data_dir(data_dir.to_path_buf());
    firecracker_terminal_capability_for(&config)
}

/// The fix for a missing or unusable guest terminal agent. Names the flag:
/// without a `temps-pty-agent` next to the running executable plain
/// `temps firecracker setup` only warns.
pub const PTY_AGENT_SETUP_COMMAND: &str = "temps firecracker setup --pty-agent-bin <path>";

fn firecracker_terminal_capability_for(
    config: &FirecrackerSandboxConfig,
) -> super::TerminalCapability {
    super::TerminalCapability::from_staged_binary(&config.pty_agent_bin(), PTY_AGENT_SETUP_COMMAND)
}

fn host_tool_available(tool: &str) -> bool {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .chain(
            ["/usr/sbin", "/sbin", "/usr/local/sbin"]
                .into_iter()
                .map(PathBuf::from),
        )
        .any(|directory| directory.join(tool).is_file())
}

fn firecracker_snapshot_host_available() -> bool {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/fuse")
        .is_ok()
        && ["mkfs.ext4", "debugfs", "bwrap", "fuse2fs", "fusermount3"]
            .into_iter()
            .all(host_tool_available)
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_missing_vmm_is_down_but_a_missed_ping_is_not_proof() {
        assert_eq!(liveness_verdict(false, false), Ok(false));
        assert_eq!(liveness_verdict(true, true), Ok(true));
        let unknown = liveness_verdict(true, false).expect_err("a missed ping is unknown");
        assert!(unknown.contains("did not answer a ping"), "{unknown}");
    }

    use super::*;

    #[test]
    fn an_agent_that_predates_a_request_is_told_apart_from_a_failing_one() {
        // Exactly what an older guest agent answers for a request it lacks.
        let parse_error =
            serde_json::from_str::<Request>(r#"{"op":"from_the_future"}"#).expect_err("unknown op");
        assert!(is_unknown_request(&format!("bad request: {parse_error}")));
        assert!(!is_unknown_request(
            "read log /var/log/temps-workload.log: Permission denied"
        ));
    }

    fn provider_at(data_dir: PathBuf) -> FirecrackerSandboxProvider {
        // Firecracker unit tests do not call Docker. An HTTP client avoids
        // requiring a local Docker socket merely to construct the provider.
        let docker = Arc::new(bollard::Docker::connect_with_http_defaults().unwrap());
        FirecrackerSandboxProvider::new(FirecrackerSandboxConfig::from_data_dir(data_dir), docker)
    }

    fn provider() -> FirecrackerSandboxProvider {
        provider_at(PathBuf::from("/nonexistent"))
    }

    #[test]
    fn resolve_name_prefers_override() {
        let p = provider();
        let mut config = SandboxCreateConfig {
            node_id: None,
            owner_user_id: None,
            run_id: 7,
            container_name_override: Some("abc123".to_string()),
            host_work_dir: PathBuf::from("/tmp"),
            workspace_volume: None,
            image: None,
            cpu_limit: None,
            memory_limit_mb: None,
            pids_limit: None,
            disk_size_mb: None,
            allow_docker: false,
            egress_policy: Default::default(),
            network_mode: None,
            env_vars: HashMap::new(),
            idle_timeout: Duration::from_secs(60),
            backend: None,
        };
        assert_eq!(p.resolve_name(&config), "temps-fcsandbox-abc123");
        config.container_name_override = None;
        assert_eq!(p.resolve_name(&config), "temps-fcsandbox-7");
    }

    // ── Rootfs injection must stay inside the image (untrusted symlinks) ──

    /// A scratch rootfs next to a "host" directory holding a file the image
    /// must never be able to touch.
    fn rootfs_with_host_file() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("rootfs");
        let host = dir.path().join("host");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&host).unwrap();
        let host_file = host.join("config");
        std::fs::write(&host_file, b"host configuration").unwrap();
        (dir, root, host, host_file)
    }

    fn staged_agent(dir: &Path, bytes: &[u8]) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let src = dir.join("agent-src");
        std::fs::write(&src, bytes).unwrap();
        std::fs::set_permissions(&src, std::fs::Permissions::from_mode(0o755)).unwrap();
        src
    }

    #[test]
    fn injection_replaces_a_symlinked_agent_path_instead_of_writing_through_it() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let (dir, root, _host, host_file) = rootfs_with_host_file();
        std::fs::create_dir_all(root.join("sbin")).unwrap();
        // The attack: the image ships the agent path as a link to a host file.
        symlink(&host_file, root.join("sbin/temps-pty-agent")).unwrap();
        let src = staged_agent(dir.path(), b"\x7fELF-agent");

        rootfs_inject(&root, Path::new("sbin/temps-pty-agent"), &src).unwrap();

        assert_eq!(std::fs::read(&host_file).unwrap(), b"host configuration");
        let injected = root.join("sbin/temps-pty-agent");
        assert!(!std::fs::symlink_metadata(&injected)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(std::fs::read(&injected).unwrap(), b"\x7fELF-agent");
        assert_eq!(
            std::fs::metadata(&injected).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }

    #[test]
    fn injection_follows_directory_symlinks_only_inside_the_rootfs() {
        use std::os::unix::fs::symlink;
        let (dir, root, host, host_file) = rootfs_with_host_file();
        // `sbin -> <absolute host dir>`: a real chroot would see it as
        // `<root><host dir>`, so the file must land there, not on the host.
        symlink(&host, root.join("sbin")).unwrap();
        let src = staged_agent(dir.path(), b"\x7fELF-agent");

        rootfs_inject(&root, Path::new("sbin/temps-vm-agent"), &src).unwrap();

        assert!(!host.join("temps-vm-agent").exists(), "escaped to the host");
        let contained = root
            .join(host.strip_prefix("/").unwrap())
            .join("temps-vm-agent");
        assert_eq!(std::fs::read(contained).unwrap(), b"\x7fELF-agent");
        assert_eq!(std::fs::read(&host_file).unwrap(), b"host configuration");
    }

    #[test]
    fn dotdot_and_relative_links_cannot_climb_out_of_the_rootfs() {
        use std::os::unix::fs::symlink;
        let (dir, root, host, _host_file) = rootfs_with_host_file();
        std::fs::create_dir_all(root.join("usr")).unwrap();
        symlink("../../../../../../host", root.join("usr/escape")).unwrap();
        symlink("usr/escape", root.join("sbin")).unwrap();
        let src = staged_agent(dir.path(), b"agent");

        let resolved = rootfs_resolve(&root, Path::new("sbin/temps-vm-agent")).unwrap();
        assert!(resolved.starts_with(&root), "{}", resolved.display());

        rootfs_inject(&root, Path::new("sbin/temps-vm-agent"), &src).unwrap();
        assert!(!host.join("temps-vm-agent").exists());
    }

    #[test]
    fn symlink_cycles_fail_instead_of_looping() {
        use std::os::unix::fs::symlink;
        let (dir, root, _host, _file) = rootfs_with_host_file();
        symlink("b", root.join("a")).unwrap();
        symlink("a", root.join("b")).unwrap();
        let src = staged_agent(dir.path(), b"agent");
        let err = rootfs_inject(&root, Path::new("a/temps-vm-agent"), &src).unwrap_err();
        assert!(err.to_string().contains("too many symbolic links"), "{err}");
    }

    #[test]
    fn scrubbing_artifacts_never_deletes_host_files() {
        use std::os::unix::fs::symlink;
        let (_dir, root, host, host_file) = rootfs_with_host_file();
        // `etc -> <host dir>` would make `remove_file(root/etc/config)`
        // delete the host's file if followed.
        symlink(&host, root.join("etc")).unwrap();
        rootfs_remove(&root, Path::new("etc/config")).unwrap();
        assert!(host_file.exists(), "a host file was deleted through a link");

        // A symlink at the final component is removed itself, not followed.
        let target = host.join("keep");
        std::fs::write(&target, b"keep").unwrap();
        std::fs::create_dir_all(root.join("etc2")).unwrap();
        symlink(&target, root.join("etc2/resolv.conf")).unwrap();
        rootfs_remove(&root, Path::new("etc2/resolv.conf")).unwrap();
        assert!(target.exists());
        assert!(!root.join("etc2/resolv.conf").exists());
        // Missing entries are fine.
        rootfs_remove(&root, Path::new("etc2/hosts")).unwrap();
    }

    #[test]
    fn injection_replaces_a_directory_squatting_on_the_agent_path() {
        let (dir, root, _host, _file) = rootfs_with_host_file();
        std::fs::create_dir_all(root.join("sbin/temps-vm-agent/nested")).unwrap();
        let src = staged_agent(dir.path(), b"agent");
        rootfs_inject(&root, Path::new("sbin/temps-vm-agent"), &src).unwrap();
        assert_eq!(
            std::fs::read(root.join("sbin/temps-vm-agent")).unwrap(),
            b"agent"
        );
    }

    // ── Terminal attach (ADR-029 §5) ─────────────────────────────────
    //
    // These speak to a stand-in for Firecracker's hybrid-vsock socket, so
    // they run anywhere — no KVM, no guest.

    /// Provider whose VM `name` has a `v.sock` served by `serve`, which gets
    /// each accepted connection. Returns the provider, its handle, and the
    /// temp dir (kept alive by the caller).
    fn provider_with_vsock<F, Fut>(
        name: &str,
        serve: F,
    ) -> (FirecrackerSandboxProvider, SandboxHandle, tempfile::TempDir)
    where
        F: FnOnce(UnixStream) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        let dir = tempfile::tempdir().unwrap();
        let provider = provider_at(dir.path().to_path_buf());
        let vm_dir = provider.config.vm_dir(name);
        std::fs::create_dir_all(&vm_dir).unwrap();
        let listener = tokio::net::UnixListener::bind(vm_dir.join("v.sock")).unwrap();
        tokio::spawn(async move {
            if let Ok((stream, _)) = listener.accept().await {
                serve(stream).await;
            }
        });
        let handle = provider.handle_for(name);
        (provider, handle, dir)
    }

    /// Read the `CONNECT <port>\n` line a client sends first.
    async fn read_connect_line(stream: &mut UnixStream) -> String {
        let mut line = Vec::new();
        loop {
            let b = stream.read_u8().await.unwrap();
            if b == b'\n' {
                break;
            }
            line.push(b);
        }
        String::from_utf8(line).unwrap()
    }

    /// Serve one RPC the way the guest agent does: the vsock handshake,
    /// then one length-prefixed JSON request answered by `respond`.
    async fn serve_agent_rpc(
        mut stream: UnixStream,
        respond: impl FnOnce(serde_json::Value) -> String,
    ) {
        assert_eq!(
            read_connect_line(&mut stream).await,
            format!("CONNECT {}", AGENT_PORT)
        );
        stream.write_all(b"OK 1073741824\n").await.unwrap();
        let len = stream.read_u32().await.unwrap();
        let mut request = vec![0u8; len as usize];
        stream.read_exact(&mut request).await.unwrap();
        let answer = respond(serde_json::from_slice(&request).unwrap());
        stream
            .write_all(&(answer.len() as u32).to_be_bytes())
            .await
            .unwrap();
        stream.write_all(answer.as_bytes()).await.unwrap();
    }

    #[tokio::test]
    async fn workload_stats_ask_for_the_process_tree_and_decode_the_sample() {
        let (provider, _handle, _dir) = provider_with_vsock("wl-stats", |stream| {
            serve_agent_rpc(stream, |request| {
                assert_eq!(request["op"], "read_stats");
                assert_eq!(request["pid"], 42);
                r#"{"result":"stats","cpu_nanos":7,"memory_bytes":8,"memory_total_bytes":9,
                    "memory_available_bytes":1,"pids":2,"disk_read_bytes":3,
                    "disk_write_bytes":4,"net_rx_bytes":5,"net_tx_bytes":6}"#
                    .to_string()
            })
        });

        let stats = provider
            .read_workload_stats("wl-stats", Some(42))
            .await
            .expect("rpc")
            .expect("a guest that knows read_stats");
        assert_eq!(stats.cpu_nanos, 7);
        assert_eq!(stats.memory_bytes, 8);
        assert_eq!(stats.memory_total_bytes, 9);
        assert_eq!(stats.pids, 2);
        assert_eq!((stats.disk_read_bytes, stats.disk_write_bytes), (3, 4));
        assert_eq!((stats.net_rx_bytes, stats.net_tx_bytes), (5, 6));
    }

    #[tokio::test]
    async fn workload_stats_from_an_old_guest_agent_are_unknown_not_an_error() {
        // An agent built before `read_stats` fails to parse the request.
        let (provider, _handle, _dir) = provider_with_vsock("wl-old", |stream| {
            serve_agent_rpc(stream, |_| {
                r#"{"result":"err","message":"bad request: unknown variant `read_stats`"}"#
                    .to_string()
            })
        });
        assert_eq!(
            provider
                .read_workload_stats("wl-old", None)
                .await
                .expect("an old agent is not a failure"),
            None
        );
    }

    #[tokio::test]
    async fn workload_stats_report_a_guest_error_with_the_vm_named() {
        let (provider, _handle, _dir) = provider_with_vsock("wl-err", |stream| {
            serve_agent_rpc(stream, |_| {
                r#"{"result":"err","message":"read stats: pid 9 was not started by spawn"}"#
                    .to_string()
            })
        });
        let error = provider
            .read_workload_stats("wl-err", Some(9))
            .await
            .expect_err("a guest error");
        let message = error.to_string();
        assert!(message.contains("wl-err"), "{message}");
        assert!(message.contains("was not started by spawn"), "{message}");
    }

    #[tokio::test]
    async fn attach_pty_handshake_that_never_answers_is_a_timeout_not_an_exec_failure() {
        // The VMM accepts the connection but the guest never acknowledges.
        let (provider, handle, _dir) = provider_with_vsock("sbx_test", |mut stream| async move {
            let _ = read_connect_line(&mut stream).await;
            // Hold the connection open without answering.
            tokio::time::sleep(Duration::from_secs(30)).await;
        });

        let started = std::time::Instant::now();
        let error = provider
            .attach_pty_within(&handle, Duration::from_millis(150))
            .await
            .err()
            .expect("an unanswered handshake must fail");

        assert!(started.elapsed() < Duration::from_secs(5));
        match error {
            AgentError::SandboxTerminalTimeout {
                sandbox_id,
                operation,
                ..
            } => {
                assert_eq!(sandbox_id, "sbx_test");
                assert!(operation.contains("vsock port"), "{operation}");
            }
            other => panic!("expected a terminal timeout, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn attach_pty_connects_to_the_pty_port_and_relays_bytes() {
        let (provider, handle, _dir) = provider_with_vsock("sbx_test", |mut stream| async move {
            assert_eq!(
                read_connect_line(&mut stream).await,
                format!("CONNECT {}", PTY_PORT)
            );
            // The agent's first bytes land immediately behind the ack, in
            // the same write — the case a buffered handshake reader loses.
            stream.write_all(b"OK 1073741824\nhello").await.unwrap();
            let mut got = [0u8; 4];
            stream.read_exact(&mut got).await.unwrap();
            assert_eq!(&got, b"ping");
            stream.write_all(b"pong").await.unwrap();
        });

        let mut attachment = provider.attach_pty(&handle).await.expect("attach");
        attachment.input.write_all(b"ping").await.unwrap();
        attachment.input.flush().await.unwrap();

        let mut received = Vec::new();
        while received.len() < "hellopong".len() {
            let chunk = attachment
                .output
                .next()
                .await
                .expect("stream ended early")
                .expect("stream error");
            received.extend_from_slice(&chunk);
        }
        assert_eq!(received, b"hellopong");
        // Server closed: the stream ends cleanly rather than erroring.
        assert!(attachment.output.next().await.is_none());
    }

    #[tokio::test]
    async fn attach_pty_without_a_running_vm_names_the_socket() {
        let dir = tempfile::tempdir().unwrap();
        let provider = provider_at(dir.path().to_path_buf());
        let handle = provider.handle_for("sbx_test");

        let err = provider.attach_pty(&handle).await.err().expect("must fail");
        let message = err.to_string();
        assert!(message.contains("sbx_test"), "{message}");
        assert!(message.contains("not running"), "{message}");
        assert!(message.contains("v.sock"), "{message}");
    }

    #[tokio::test]
    async fn attach_pty_explains_a_guest_that_has_no_terminal_listener() {
        // The VMM drops a connection to a vsock port nobody listens on.
        let (provider, handle, _dir) = provider_with_vsock("sbx_test", |mut stream| async move {
            let _ = read_connect_line(&mut stream).await;
            drop(stream);
        });

        let err = provider.attach_pty(&handle).await.err().expect("must fail");
        let message = err.to_string();
        assert!(message.contains("sbx_test"), "{message}");
        assert!(message.contains(&PTY_PORT.to_string()), "{message}");
        // No agent staged on this host: the remedy is the setup command.
        assert!(message.contains("temps firecracker setup"), "{message}");
        assert!(message.contains("--pty-agent-bin"), "{message}");
    }

    #[tokio::test]
    async fn attach_pty_on_an_older_guest_says_to_recreate_it() {
        let (provider, handle, dir) = provider_with_vsock("sbx_test", |mut stream| async move {
            let _ = read_connect_line(&mut stream).await;
            drop(stream);
        });
        // The host has the agent staged now; this VM predates it.
        let staged = provider.config.pty_agent_bin();
        std::fs::create_dir_all(staged.parent().unwrap()).unwrap();
        std::fs::write(&staged, crate::sandbox::test_elf::static_for_host(b"")).unwrap();
        let _keep = dir;

        let message = provider
            .attach_pty(&handle)
            .await
            .err()
            .expect("must fail")
            .to_string();
        assert!(message.contains("recreate the sandbox"), "{message}");
        assert!(!message.contains("--pty-agent-bin"), "{message}");
    }

    #[tokio::test]
    async fn attach_pty_rejects_a_refused_handshake() {
        let (provider, handle, _dir) = provider_with_vsock("sbx_test", |mut stream| async move {
            let _ = read_connect_line(&mut stream).await;
            stream.write_all(b"ERR connection refused\n").await.unwrap();
        });
        let err = provider.attach_pty(&handle).await.err().expect("must fail");
        assert!(err.to_string().contains("handshake"), "{err}");
    }

    #[tokio::test]
    async fn rpc_still_round_trips_through_the_shared_handshake() {
        // `rpc` and `attach_pty` share `vsock_connect`; the refactor must
        // leave the RPC path byte-identical on the wire.
        let (provider, _handle, _dir) = provider_with_vsock("sbx_test", |mut stream| async move {
            assert_eq!(
                read_connect_line(&mut stream).await,
                format!("CONNECT {}", AGENT_PORT)
            );
            stream.write_all(b"OK 1073741824\n").await.unwrap();
            let len = stream.read_u32().await.unwrap() as usize;
            let mut body = vec![0u8; len];
            stream.read_exact(&mut body).await.unwrap();
            let request: Request = serde_json::from_slice(&body).unwrap();
            assert!(matches!(request, Request::Ping));
            let reply = serde_json::to_vec(&Response::Pong).unwrap();
            stream
                .write_all(&(reply.len() as u32).to_be_bytes())
                .await
                .unwrap();
            stream.write_all(&reply).await.unwrap();
        });

        let response = provider
            .rpc("sbx_test", &Request::Ping, Duration::from_secs(5))
            .await
            .expect("rpc");
        assert!(matches!(response, Response::Pong));
    }

    #[test]
    fn an_unusable_optional_agent_never_blocks_sandbox_creation() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let p = provider_at(dir.path().to_path_buf());
        let bin = dir.path().join("firecracker/bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("temps-vm-agent"), b"vm-1").unwrap();
        let without_pty = p.guest_agent_fingerprint().unwrap();

        // Unreadable (a root-owned 0600 file staged by sudo, say): the same
        // key as "not staged", because it is injected in neither case.
        let staged = bin.join("temps-pty-agent");
        std::fs::write(&staged, crate::sandbox::test_elf::static_for_host(b"x")).unwrap();
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o000)).unwrap();
        let unreadable = p.guest_agent_fingerprint();
        // Running as root bypasses file modes; there the file is simply read.
        if std::fs::File::open(&staged).is_err() {
            assert_eq!(unreadable.unwrap(), without_pty);
        } else {
            assert!(unreadable.is_ok());
        }
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o644)).unwrap();

        // A foreign build (Mach-O) is not injected either, so it must not
        // churn the cache key or fail creation.
        std::fs::write(&staged, [0xcf, 0xfa, 0xed, 0xfe, 0, 0, 0, 0]).unwrap();
        assert_eq!(p.guest_agent_fingerprint().unwrap(), without_pty);
    }

    #[test]
    fn guest_fingerprint_tracks_both_injected_agents() {
        let dir = tempfile::tempdir().unwrap();
        let p = provider_at(dir.path().to_path_buf());
        let bin = dir.path().join("firecracker/bin");
        std::fs::create_dir_all(&bin).unwrap();

        // The VM agent is mandatory.
        let err = p.guest_agent_fingerprint().unwrap_err().to_string();
        assert!(err.contains("temps firecracker setup"), "{err}");

        std::fs::write(bin.join("temps-vm-agent"), b"vm-1").unwrap();
        let without_pty = p.guest_agent_fingerprint().unwrap();
        assert_eq!(without_pty.len(), 12);

        // Staging the PTY agent must change the cache key, or a rootfs
        // converted before it existed would be reused with no terminals.
        std::fs::write(
            bin.join("temps-pty-agent"),
            crate::sandbox::test_elf::static_for_host(b"pty-1"),
        )
        .unwrap();
        let with_pty = p.guest_agent_fingerprint().unwrap();
        assert_ne!(without_pty, with_pty);

        std::fs::write(
            bin.join("temps-pty-agent"),
            crate::sandbox::test_elf::static_for_host(b"pty-2"),
        )
        .unwrap();
        assert_ne!(with_pty, p.guest_agent_fingerprint().unwrap());

        std::fs::write(bin.join("temps-vm-agent"), b"vm-2").unwrap();
        assert_ne!(with_pty, p.guest_agent_fingerprint().unwrap());
    }

    #[tokio::test]
    async fn recover_by_name_accepts_bare_label() {
        let p = provider();
        // Nonexistent data dir → no VM dir → None either way, but both
        // spellings must be accepted without panicking.
        assert!(p.recover_by_name("abc").await.unwrap().is_none());
        assert!(p
            .recover_by_name("temps-fcsandbox-abc")
            .await
            .unwrap()
            .is_none());
    }

    #[test]
    fn snapshot_scrub_preserves_workspace_and_removes_runtime_secrets() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("workspace/src")).unwrap();
        std::fs::create_dir_all(root.path().join("root/.claude")).unwrap();
        std::fs::create_dir_all(root.path().join("etc/temps")).unwrap();
        std::fs::write(root.path().join("workspace/src/state.txt"), b"preserve").unwrap();
        std::fs::write(root.path().join("root/.claude/token"), b"secret").unwrap();
        std::fs::write(
            root.path().join("etc/temps/credential-daemon.env"),
            b"TOKEN=secret",
        )
        .unwrap();

        FirecrackerSandboxProvider::scrub_snapshot_tree(root.path()).unwrap();

        assert_eq!(
            std::fs::read(root.path().join("workspace/src/state.txt")).unwrap(),
            b"preserve"
        );
        assert!(!root.path().join("root/.claude/token").exists());
        assert!(!root.path().join("etc/temps/credential-daemon.env").exists());
    }

    #[test]
    fn snapshot_size_walk_does_not_follow_guest_symlinks() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("outside.bin"), vec![1u8; 64 * 1024]).unwrap();
        symlink(outside.path(), root.path().join("escape")).unwrap();

        let measured = dir_size(root.path());

        assert!(measured < 64 * 1024);
    }

    fn args_of(command: &tokio::process::Command) -> Vec<String> {
        command
            .as_std()
            .get_args()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect()
    }

    #[test]
    fn a_guest_image_is_checked_and_resized_with_only_that_image_visible() {
        let image = PathBuf::from("/data/vms/app/rootfs.ext4");
        let command =
            FirecrackerSandboxProvider::sandboxed_ext4_tool_command(&image, "e2fsck", &["-fy"]);
        assert_eq!(command.as_std().get_program(), "bwrap");
        let args = args_of(&command);
        let position = |needle: &[&str]| {
            args.windows(needle.len())
                .position(|window| window.iter().map(String::as_str).eq(needle.iter().copied()))
        };
        // No network, no host procfs or devices, no capabilities, read-only root.
        assert!(position(&["--unshare-all", "--die-with-parent"]).is_some());
        assert!(position(&["--cap-drop", "ALL"]).is_some());
        assert!(position(&["--remount-ro", "/"]).is_some());
        assert!(!args
            .iter()
            .any(|a| a == "--proc" || a == "--dev" || a == "--share-net"));
        // The image is the only thing bound writable, after the root went read-only.
        let image_bind = position(&["--bind", "/data/vms/app/rootfs.ext4", "/tmp/rootfs.ext4"])
            .expect("the image is bound");
        assert!(position(&["--remount-ro", "/"]).unwrap() < image_bind);
        assert_eq!(args.iter().filter(|a| *a == "--bind").count(), 1);
        assert!(
            !args
                .iter()
                .any(|a| a.contains("/data/vms") && a != "/data/vms/app/rootfs.ext4"),
            "nothing else of the data directory is exposed"
        );
        // The tool names the in-sandbox path, never the host's.
        let tail: Vec<&str> = args.iter().rev().take(3).map(String::as_str).collect();
        assert_eq!(tail, vec!["/tmp/rootfs.ext4", "-fy", "e2fsck"]);
    }

    #[test]
    fn bubblewrap_start_failures_say_what_to_change() {
        assert!(
            bubblewrap_hint("bwrap: No permissions to create new namespace")
                .contains("apparmor_restrict_unprivileged_userns")
        );
        assert!(bubblewrap_hint("Unexpected capabilities but not setuid")
            .contains("AmbientCapabilities"));
        assert_eq!(bubblewrap_hint("e2fsck: bad magic"), "");
    }

    #[tokio::test]
    async fn growing_an_image_runs_fsck_and_resize_in_the_sandbox_and_keeps_data() {
        for tool in ["mkfs.ext4", "e2fsck", "resize2fs", "bwrap"] {
            if tokio::process::Command::new(tool)
                .arg("--help")
                .output()
                .await
                .is_err()
            {
                eprintln!("skipping the sandboxed grow test: '{tool}' is unavailable");
                return;
            }
        }
        // bwrap can be installed yet forbidden user namespaces.
        let probe = tokio::process::Command::new("bwrap")
            .args(["--unshare-all", "--ro-bind", "/", "/", "true"])
            .output()
            .await;
        if !probe.map(|o| o.status.success()).unwrap_or(false) {
            eprintln!("skipping the sandboxed grow test: bubblewrap cannot start here");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let provider = provider_at(dir.path().to_path_buf());
        let tree = dir.path().join("tree");
        std::fs::create_dir_all(&tree).unwrap();
        std::fs::write(tree.join("kept.txt"), b"survives the resize").unwrap();
        let image = dir.path().join("rootfs.ext4");
        let mkfs = tokio::process::Command::new("mkfs.ext4")
            .args(["-q", "-F", "-d"])
            .arg(&tree)
            .arg(&image)
            .arg("16384")
            .output()
            .await
            .unwrap();
        assert!(
            mkfs.status.success(),
            "{}",
            String::from_utf8_lossy(&mkfs.stderr)
        );

        provider
            .grow_rootfs(&image, 32 * 1024 * 1024)
            .await
            .expect("grown inside the sandbox");
        assert_eq!(std::fs::metadata(&image).unwrap().len(), 32 * 1024 * 1024);
        // The filesystem is consistent and larger.
        let check = tokio::process::Command::new("e2fsck")
            .args(["-fn"])
            .arg(&image)
            .output()
            .await
            .unwrap();
        assert!(
            check.status.success(),
            "{}",
            String::from_utf8_lossy(&check.stdout)
        );
    }

    #[tokio::test]
    async fn growing_a_corrupt_image_fails_naming_the_tool() {
        for tool in ["e2fsck", "bwrap"] {
            if tokio::process::Command::new(tool)
                .arg("--help")
                .output()
                .await
                .is_err()
            {
                eprintln!("skipping: '{tool}' is unavailable");
                return;
            }
        }
        let probe = tokio::process::Command::new("bwrap")
            .args(["--unshare-all", "--ro-bind", "/", "/", "true"])
            .output()
            .await;
        if !probe.map(|o| o.status.success()).unwrap_or(false) {
            eprintln!("skipping: bubblewrap cannot start here");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let provider = provider_at(dir.path().to_path_buf());
        let image = dir.path().join("garbage.ext4");
        std::fs::write(&image, vec![0xA5u8; 1024 * 1024]).unwrap();
        let error = provider
            .grow_rootfs(&image, 4 * 1024 * 1024)
            .await
            .expect_err("garbage is not a filesystem");
        assert!(error.to_string().contains("e2fsck"), "{error}");
    }

    #[test]
    fn snapshot_extractor_uses_minimal_bubblewrap_namespace() {
        let staging = PathBuf::from("/safe/staging");
        let source = PathBuf::from("/safe/source.ext4");
        let command = FirecrackerSandboxProvider::sandboxed_debugfs_command(&staging, &source);
        let std_command = command.as_std();
        assert_eq!(std_command.get_program(), "bwrap");
        let args = std_command
            .get_args()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect::<Vec<_>>();

        let has_sequence = |expected: &[&str]| {
            args.windows(expected.len()).any(|window| {
                window
                    .iter()
                    .map(String::as_str)
                    .eq(expected.iter().copied())
            })
        };
        assert!(has_sequence(&["--unshare-all", "--die-with-parent"]));
        assert!(has_sequence(&["--dir", "/dev"]));
        assert!(has_sequence(&["--dir", "/run"]));
        assert!(has_sequence(&["--dir", "/tmp"]));
        assert!(has_sequence(&["--remount-ro", "/"]));
        assert!(has_sequence(&["--bind", "/safe/staging", "/tmp"]));
        assert!(has_sequence(&[
            "--ro-bind",
            "/safe/source.ext4",
            "/tmp/source.ext4"
        ]));
        assert!(
            !args.iter().any(|arg| arg == "--proc"),
            "procfs would let malicious dirents target inherited file descriptors"
        );
        assert!(
            !args.iter().any(|arg| arg == "--dev"),
            "the extractor must use an empty /dev instead of host-backed devices"
        );
        assert!(
            !args.iter().any(|arg| arg == "--tmpfs"),
            "a writable tmpfs outside staging would bypass snapshot quotas"
        );
        let root_read_only = args
            .windows(2)
            .position(|window| window == ["--remount-ro", "/"])
            .unwrap();
        let staging_bind = args
            .windows(3)
            .position(|window| window == ["--bind", "/safe/staging", "/tmp"])
            .unwrap();
        assert!(
            root_read_only < staging_bind,
            "only the staging bind may remain writable after the root is remounted read-only"
        );
        assert!(
            !has_sequence(&["--ro-bind", "/", "/"]),
            "the untrusted extractor must never see the host root"
        );
        for hidden_host_path in ["/dev", "/proc", "/sys", "/run", "/etc", "/home", "/root"] {
            assert!(
                !has_sequence(&["--ro-bind", hidden_host_path, hidden_host_path]),
                "host path {hidden_host_path} must not be exposed read-only to the parser"
            );
        }
    }

    #[tokio::test]
    async fn snapshot_extractor_timeout_kills_the_child() {
        let mut command = tokio::process::Command::new("sleep");
        command.arg("30");

        let error = FirecrackerSandboxProvider::run_snapshot_extractor(
            &mut command,
            Duration::from_millis(20),
        )
        .await
        .unwrap_err();

        assert!(matches!(error, SnapshotExtractorError::TimedOut { .. }));
    }

    #[tokio::test]
    async fn snapshot_extractor_reports_a_bounded_stderr_head() {
        let mut command = tokio::process::Command::new("sh");
        command
            .arg("-c")
            .arg("echo first-line >&2; head -c 100000 /dev/zero | tr '\\0' x >&2; exit 3");

        let output = FirecrackerSandboxProvider::run_snapshot_extractor(
            &mut command,
            Duration::from_secs(10),
        )
        .await
        .unwrap();

        assert_eq!(output.status.code(), Some(3));
        assert!(output.stderr.starts_with("first-line"), "{}", output.stderr);
        assert!(output.stderr.len() <= SNAPSHOT_EXTRACTOR_STDERR_BYTES);
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn extractor_children_start_without_ambient_capabilities() {
        // The hook must not break exec, and the child must report an empty
        // ambient set. (The set is already empty under test; the systemd
        // case is covered by the hook being applied to the bwrap command.)
        let mut command = tokio::process::Command::new("sh");
        command
            .arg("-c")
            .arg("grep -q '^CapAmb:[[:space:]]*0*$' /proc/self/status");
        FirecrackerSandboxProvider::without_ambient_capabilities(&mut command);

        let output = FirecrackerSandboxProvider::run_snapshot_extractor(
            &mut command,
            Duration::from_secs(10),
        )
        .await
        .unwrap();

        assert!(output.status.success(), "{:?}", output);
    }

    #[tokio::test]
    async fn export_failure_message_carries_the_tool_output() {
        let mut command = tokio::process::Command::new("sh");
        command.arg("-c").arg("exit 1");
        let status = command.status().await.unwrap();

        let silent = snapshot_export_failure_message(status, "");
        assert!(silent.contains("it printed nothing"), "{silent}");

        let said = snapshot_export_failure_message(status, "debugfs: Bad magic number");
        assert!(
            said.contains("it said: debugfs: Bad magic number"),
            "{said}"
        );
        assert!(!said.contains("AppArmor"), "{said}");

        let userns = snapshot_export_failure_message(
            status,
            "bwrap: No permissions to create new namespace",
        );
        assert!(
            userns.contains("apparmor_restrict_unprivileged_userns"),
            "{userns}"
        );
    }

    #[test]
    fn disconnected_fuse_mount_errors_require_lazy_unmount() {
        for raw_error in [libc::ENOTCONN, libc::EIO] {
            let error = std::io::Error::from_raw_os_error(raw_error);
            assert!(FuseSnapshotStaging::is_disconnected_mount_error(&error));
        }
        let missing = std::io::Error::from(std::io::ErrorKind::NotFound);
        assert!(!FuseSnapshotStaging::is_disconnected_mount_error(&missing));
    }

    #[tokio::test]
    async fn sparse_guest_file_cannot_exceed_snapshot_staging_quota() {
        if !cfg!(target_os = "linux") {
            eprintln!("skipping bounded Firecracker extraction test: Linux is required");
            return;
        }
        if std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/fuse")
            .is_err()
        {
            eprintln!("skipping bounded Firecracker extraction test: /dev/fuse is unavailable");
            return;
        }
        for tool in ["mkfs.ext4", "debugfs", "bwrap", "fuse2fs", "fusermount3"] {
            if tokio::process::Command::new(tool)
                .arg("--help")
                .output()
                .await
                .is_err()
            {
                eprintln!(
                    "skipping bounded Firecracker extraction test: '{}' is unavailable",
                    tool
                );
                return;
            }
        }

        let data_dir = tempfile::tempdir().unwrap();
        let provider = provider_at(data_dir.path().to_path_buf());
        let source_tree = data_dir.path().join("sparse-source-tree");
        std::fs::create_dir_all(source_tree.join("workspace")).unwrap();
        let sparse_path = source_tree.join("workspace/oversized-sparse.bin");
        let sparse_file = std::fs::File::create(&sparse_path).unwrap();
        sparse_file.set_len(256 * 1024 * 1024).unwrap();
        drop(sparse_file);

        let source_rootfs = data_dir.path().join("sparse-source.ext4");
        let mkfs = tokio::process::Command::new("mkfs.ext4")
            .args(["-q", "-F", "-d"])
            .arg(&source_tree)
            .arg(&source_rootfs)
            .arg("131072")
            .output()
            .await
            .unwrap();
        assert!(
            mkfs.status.success(),
            "mkfs.ext4 failed: {}",
            String::from_utf8_lossy(&mkfs.stderr)
        );

        let max_size_bytes = 96 * 1024 * 1024;
        assert!(
            FirecrackerSandboxProvider::try_disk_bytes(&source_rootfs).unwrap() < max_size_bytes,
            "the sparse guest image must pass the allocated-block precheck"
        );
        let destination = data_dir.path().join("bounded-snapshot.ext4");

        let error = provider
            .export_sanitized_rootfs(
                &source_rootfs,
                &destination,
                max_size_bytes,
                "sparse-quota-regression",
            )
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            AgentError::SnapshotSizeLimitExceeded { .. }
        ));
        assert!(!destination.exists());
        assert!(!FirecrackerSandboxProvider::path_with_suffix(&destination, ".staging").exists());
        assert!(
            !FirecrackerSandboxProvider::path_with_suffix(&destination, ".extractor.ext4").exists()
        );
    }

    #[tokio::test]
    async fn failed_firecracker_restore_removes_copied_vm_directory() {
        let data_dir = tempfile::tempdir().unwrap();
        let provider = provider_at(data_dir.path().to_path_buf());
        let snapshot_path = data_dir.path().join("snapshot.ext4");
        std::fs::write(&snapshot_path, b"synthetic-rootfs").unwrap();
        let digest = FirecrackerSandboxProvider::hash_file(&snapshot_path).unwrap();
        let artifact = super::super::SnapshotArtifact {
            content_path: snapshot_path,
            content_digest: digest.clone(),
            primary_digest: digest,
            size_bytes: 16,
            backend: super::super::SandboxBackend::Firecracker,
            image_ref: None,
            image_id: None,
            workspace: None,
        };
        let label = "failed-restore-cleanup";
        let config = SandboxCreateConfig {
            node_id: None,
            owner_user_id: None,
            run_id: 17,
            container_name_override: Some(label.to_string()),
            host_work_dir: data_dir.path().join("unused-workspace"),
            workspace_volume: None,
            image: None,
            cpu_limit: None,
            memory_limit_mb: None,
            pids_limit: None,
            disk_size_mb: Some(0),
            allow_docker: false,
            egress_policy: Default::default(),
            network_mode: None,
            env_vars: HashMap::new(),
            idle_timeout: Duration::from_secs(60),
            backend: Some(super::super::SandboxBackend::Firecracker),
        };

        let result = provider.create_from_snapshot(&artifact, config).await;

        assert!(result.is_err());
        assert!(!provider
            .config
            .vm_dir(&format!("{}{}", FC_SANDBOX_NAME_PREFIX, label))
            .exists());
    }

    #[tokio::test]
    async fn firecracker_snapshot_rootfs_round_trip_preserves_workspace_bytes() {
        if !cfg!(target_os = "linux") {
            eprintln!("skipping Firecracker snapshot test: Linux is required");
            return;
        }
        if std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/fuse")
            .is_err()
        {
            eprintln!("skipping Firecracker snapshot test: /dev/fuse is unavailable");
            return;
        }
        for tool in ["mkfs.ext4", "debugfs", "bwrap", "fuse2fs", "fusermount3"] {
            if tokio::process::Command::new(tool)
                .arg("--help")
                .output()
                .await
                .is_err()
            {
                eprintln!(
                    "skipping Firecracker snapshot test: '{}' is unavailable",
                    tool
                );
                return;
            }
        }

        let data_dir = tempfile::tempdir().unwrap();
        let provider = provider_at(data_dir.path().to_path_buf());
        let sandbox_name = "temps-fcsandbox-snapshot-round-trip";
        let vm_dir = provider.config.vm_dir(sandbox_name);
        std::fs::create_dir_all(&vm_dir).unwrap();
        let source_rootfs = vm_dir.join("rootfs.ext4");
        let source_tree = data_dir.path().join("source-tree");
        std::fs::create_dir_all(source_tree.join("workspace/src")).unwrap();
        std::fs::create_dir_all(source_tree.join("root/.claude")).unwrap();
        let workspace_state = b"workspace-state-must-survive";
        let injected_secret = b"tok-injected-secret-must-not-survive";
        std::fs::write(source_tree.join("workspace/src/state.txt"), workspace_state).unwrap();
        std::fs::write(
            source_tree.join("root/.claude/credentials.json"),
            injected_secret,
        )
        .unwrap();
        let mkfs = tokio::process::Command::new("mkfs.ext4")
            .args(["-q", "-F", "-d"])
            .arg(&source_tree)
            .arg(&source_rootfs)
            .arg("32768")
            .output()
            .await
            .unwrap();
        assert!(
            mkfs.status.success(),
            "mkfs.ext4 failed: {}",
            String::from_utf8_lossy(&mkfs.stderr)
        );
        let handle = provider.handle_with_image(sandbox_name, "alpine:3.20".to_string());

        let artifact = provider
            .take_snapshot(&handle, None, 512 * 1024 * 1024)
            .await
            .unwrap();

        assert_eq!(artifact.backend, super::super::SandboxBackend::Firecracker);
        assert!(artifact.workspace.is_none());

        let workspace = tokio::process::Command::new("debugfs")
            .args(["-R", "cat /workspace/src/state.txt"])
            .arg(&artifact.content_path)
            .output()
            .await
            .unwrap();
        assert_eq!(workspace.stdout, workspace_state);

        let artifact_bytes = std::fs::read(&artifact.content_path).unwrap();
        assert!(
            !artifact_bytes
                .windows(injected_secret.len())
                .any(|window| window == injected_secret),
            "sanitized snapshot must not retain injected token bytes, including in free blocks"
        );
    }
}
