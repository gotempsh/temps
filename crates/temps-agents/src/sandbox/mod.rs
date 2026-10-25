// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

pub mod docker;
pub mod driver;
pub mod facade;
pub mod firecracker;
pub mod git_credential_bundle;
pub mod guest_agent_build;
pub mod local;
pub mod managed;
pub mod microsandbox;
pub mod node_routing;
pub mod pty_agent_bundle;
pub mod remote;
pub mod routing;
pub mod user;
pub mod workload_vm;

/// The Docker named volume holding `/home/temps` for a sandbox whose
/// container suffix is `label`.
///
/// Public because the standalone sandbox service derives that label itself
/// (it strips `sbx_` off the `public_id`) and a test needs to prove the two
/// crates still agree — the leak this replaced was precisely two places
/// deriving one identity independently. Production code goes through
/// `DockerSandboxProvider::sandbox_names`.
pub fn home_volume_name_for_label(label: &str) -> String {
    format!(
        "{}{}{}",
        docker::HOME_VOLUME_PREFIX,
        docker::HOME_VOLUME_SCHEME,
        label
    )
}

/// A sandbox's outbound policy (which hosts and CIDR blocks it may reach).
pub use temps_workload::{
    EgressMode, EgressPolicyError, EgressRule, SandboxEgressPolicy, MAX_EGRESS_RULES,
};

pub use user::{
    SANDBOX_CHOWN, SANDBOX_GID, SANDBOX_GROUP, SANDBOX_HOME, SANDBOX_UID, SANDBOX_USER,
    SANDBOX_WORK_DIR,
};

use async_trait::async_trait;
use futures::Stream;
use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWrite;

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
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "lowercase")]
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

/// Which isolation backend a sandbox runs on (ADR-029, ADR-050). Docker
/// containers, Firecracker microVMs and microsandbox (libkrun) microVMs
/// coexist on the same host behind the same `SandboxProvider` seam;
/// `routing::RoutingSandboxProvider` dispatches between them. `Local` is the
/// dev-only fork-exec fallback.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum SandboxBackend {
    Docker,
    Firecracker,
    /// Experimental libkrun microVM backend (ADR-050).
    Microsandbox,
    Local,
}

impl std::str::FromStr for SandboxBackend {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "docker" => Ok(Self::Docker),
            "firecracker" => Ok(Self::Firecracker),
            "microsandbox" => Ok(Self::Microsandbox),
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
            Self::Microsandbox => "microsandbox",
            Self::Local => "local",
        })
    }
}

/// A handle to an active sandbox. Opaque to callers — the internal fields
/// are provider-specific (Docker container ID, Vercel sandbox ID, etc.).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct SandboxHandle {
    /// Worker node that hosts this sandbox (ADR-048). `None` = the local
    /// host (control plane). Stamped by [`node_routing::NodeRoutingSandboxProvider`]
    /// so every handle-based call is dispatched to the node that owns the
    /// sandbox; concrete providers always leave it `None`.
    #[serde(default)]
    pub node_id: Option<i32>,
    /// Provider-specific identifier (container ID, Vercel sandbox ID, etc.)
    pub sandbox_id: String,
    /// Human-readable name for logging (e.g. `temps-sandbox-42`)
    pub sandbox_name: String,
    /// Path to the repository inside the sandbox. See `sandbox::user::SANDBOX_WORK_DIR`.
    #[schema(value_type = String)]
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
    /// What the driver facade found when it placed the sandbox (ADR-055
    /// §5, §6): `LimitEnforced = False` with the reason when a requested
    /// limit could not be applied, and `Scheduled` when the preferred
    /// driver was passed over. Only the facade sets it; empty on the
    /// single-backend providers it wraps, and then omitted from the wire so
    /// a handle without conditions serializes as it always did.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schema(value_type = Vec<Object>)]
    pub conditions: Vec<temps_workload::Condition>,
}

/// Configuration for creating a new sandbox.
pub struct SandboxCreateConfig {
    /// Permit guest Docker installation by retaining normal guest-root semantics.
    /// Only microsandbox supports this opt-in; never grants host Docker access.
    pub allow_docker: bool,
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
    /// Which destinations the sandbox may reach once it has a network. The
    /// default (`internet`) is any public destination on ports 80/443; a
    /// `restricted` policy needs a backend with egress allow-lists (Docker)
    /// and is refused with [`AgentError::SandboxEgressPolicyUnsupported`]
    /// elsewhere.
    pub egress_policy: SandboxEgressPolicy,
    /// Environment variables to inject (ANTHROPIC_API_KEY, etc.)
    pub env_vars: HashMap<String, String>,
    /// Maximum time the sandbox should stay alive without activity
    pub idle_timeout: Duration,
    /// Isolation backend for this sandbox. `None` = the host's configured
    /// default. Only meaningful when the registered provider is the
    /// routing provider; single-backend hosts ignore it.
    pub backend: Option<SandboxBackend>,
    /// User to attribute the sandbox to in the standalone sandbox API
    /// (`sandboxes.user_id`). Only read by the managed run-sandbox path
    /// ([`managed::RunSandboxService`]); providers ignore it. `None` for
    /// webhook-triggered runs with no acting user.
    pub owner_user_id: Option<i32>,
    /// Worker node to create the sandbox on (ADR-048). `None` = the local
    /// host. Only read by [`node_routing::NodeRoutingSandboxProvider`];
    /// concrete providers ignore it.
    pub node_id: Option<i32>,
}

impl SandboxCreateConfig {
    /// Fail closed rather than silently ignoring an unsupported guest privilege request.
    pub(crate) fn validate_allow_docker(&self, backend: SandboxBackend) -> Result<(), AgentError> {
        if self.allow_docker && backend != SandboxBackend::Microsandbox {
            return Err(AgentError::Validation {
                message: format!(
                    "sandbox run {}: allow_docker requires the microsandbox backend; {} does not support guest Docker",
                    self.run_id, backend
                ),
            });
        }
        Ok(())
    }

    /// The name a create request goes by in errors: the explicit label, or
    /// the run id.
    pub(crate) fn sandbox_label(&self) -> String {
        self.container_name_override
            .clone()
            .unwrap_or_else(|| self.run_id.to_string())
    }

    /// Fail closed on a backend that cannot enforce a restricted egress
    /// policy, rather than starting a sandbox with more access than asked.
    pub(crate) fn refuse_restricted_egress(
        &self,
        backend: SandboxBackend,
    ) -> Result<(), AgentError> {
        if self.egress_policy.is_restricted() {
            return Err(AgentError::SandboxEgressPolicyUnsupported {
                sandbox: self.sandbox_label(),
                backend: backend.to_string(),
            });
        }
        Ok(())
    }
}

/// The error [`SandboxProvider::read_file_bounded`] returns for a file over
/// its limit.
pub(crate) fn file_too_large(handle: &SandboxHandle, path: &str, max_bytes: u64) -> AgentError {
    AgentError::Validation {
        message: format!(
            "read_file: '{}' in sandbox {} is larger than the {} byte ({} MiB) limit for this read",
            path,
            handle.sandbox_name,
            max_bytes,
            max_bytes / (1024 * 1024)
        ),
    }
}

fn direct_model_relay_base_url(control_plane_url: &str) -> String {
    format!(
        "{}/api/ai/sandbox-models",
        control_plane_url.trim_end_matches('/')
    )
}

/// Artifact produced by a successful [`SandboxProvider::take_snapshot`] call.
///
/// Contains everything the service layer needs to persist the snapshot row and
/// to re-hydrate the snapshot on restore. It is a plain data struct, not a
/// trait object — no provider-specific types leak across the boundary.
///
/// ADR-037 §2 describes the full design.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct SnapshotCompanionArtifact {
    /// Absolute path to the companion artifact on the host.
    pub content_path: std::path::PathBuf,
    /// SHA-256 hex of this companion file.
    pub content_digest: String,
    /// Size of this companion file in bytes.
    pub size_bytes: u64,
}

#[derive(Debug, Clone)]
pub struct SnapshotArtifact {
    /// Content-addressed path on the host. Docker stores an image tarball;
    /// Firecracker stores a sanitized sparse ext4 filesystem. Providers write
    /// to a temporary path and publish the artifact atomically.
    pub content_path: std::path::PathBuf,
    /// SHA-256 hex of the complete logical snapshot state. The canonical
    /// store key — two snapshots with identical content share files on disk.
    pub content_digest: String,
    /// SHA-256 of the primary artifact file. For a single-file snapshot this
    /// equals `content_digest`; multi-file snapshots use `content_digest` as
    /// the combined logical key and retain this checksum for restore-time
    /// verification.
    pub primary_digest: String,
    /// Total size of the primary artifact and every companion artifact.
    pub size_bytes: u64,
    /// Which backend produced this artifact. Cross-backend restore is rejected
    /// with 422 at the service layer (a Docker tar cannot boot as a Firecracker
    /// rootfs, and vice versa).
    pub backend: SandboxBackend,
    /// For Docker: the daemon image tag `temps-snapshot/<logical_digest>:latest`
    /// that was committed before exporting. Stored so the restore path can
    /// confirm (or re-import) the image in the daemon without parsing the
    /// tarball name.
    pub image_ref: Option<String>,
    /// Immutable Docker image ID captured before export. Restore verifies that
    /// a mutable `image_ref` still resolves to this ID before using it.
    pub image_id: Option<String>,
    /// Workspace bytes stored separately from the provider's primary
    /// artifact. Docker needs this because its workspace is a bind mount and
    /// `docker commit` deliberately excludes mounts. Firecracker leaves this
    /// as `None` because its sanitized ext4 artifact contains the workspace.
    pub workspace: Option<SnapshotCompanionArtifact>,
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

/// Read-only compatibility result for the retained SDK runtime transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeCompatibility {
    Compatible,
    Unavailable { reason: String },
    Incompatible { reason: String },
}

/// A live bidirectional byte channel into a sandbox's PTY agent.
///
/// Returned by [`SandboxProvider::attach_pty`]. The two halves are separate
/// so a caller can split them across tasks — a terminal needs to pump both
/// directions concurrently, which a single `AsyncRead + AsyncWrite` object
/// makes awkward behind `dyn`.
///
/// The bytes are `temps-pty-agent`'s framed protocol (see ADR-008), not raw
/// PTY output. Framing is the caller's job; the provider only moves bytes.
pub struct PtyAttachment {
    /// Frames from the agent. Ends when the underlying exec exits.
    pub output: Pin<Box<dyn Stream<Item = Result<bytes::Bytes, AgentError>> + Send>>,
    /// Frames to the agent. Dropping this closes the agent-side stdin,
    /// which makes the in-container relay exit and the attach terminate.
    pub input: Pin<Box<dyn AsyncWrite + Send>>,
}

/// Absolute path of the PTY agent's control socket inside every sandbox.
///
/// Fixed by the sandbox Dockerfile (`RUN mkdir -p /run/temps-pty …`), so it
/// is a property of the image rather than something a caller chooses.
pub const PTY_AGENT_SOCKET: &str = "/run/temps-pty/agent.sock";

/// File name of the static Linux `temps-pty-agent` build that the microVM
/// backends inject into guests. `temps firecracker setup` and
/// `temps microsandbox setup` look for it next to the running executable
/// (release archives ship it there) unless told otherwise.
pub const PTY_AGENT_BINARY_NAME: &str = "temps-pty-agent";

/// Whether interactive terminals (`SandboxProvider::attach_pty`) can be
/// opened on a backend right now, and if not, what to do about it.
///
/// Reported next to each backend's availability so the console and CLI can
/// say "terminals need X" up front instead of discovering it as a bare 409
/// when someone opens one.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
pub struct TerminalCapability {
    /// Whether a terminal can be attached to a sandbox on this backend.
    pub supported: bool,
    /// Why not, when `supported` is false.
    pub reason: Option<String>,
    /// Shell command that fixes it, when running one is the fix.
    pub setup_command: Option<String>,
}

impl TerminalCapability {
    pub fn supported() -> Self {
        Self {
            supported: true,
            reason: None,
            setup_command: None,
        }
    }

    pub fn unsupported(reason: impl Into<String>, setup_command: Option<&str>) -> Self {
        Self {
            supported: false,
            reason: Some(reason.into()),
            setup_command: setup_command.map(str::to_string),
        }
    }

    /// Capability for a microVM backend whose guests get `temps-pty-agent`
    /// injected from a binary staged on the host at `staged`.
    ///
    /// A binary that is not a Linux ELF (for instance the macOS build of
    /// `temps-pty-agent` copied next to a development `temps`) is reported
    /// as unusable rather than discovered when a guest fails to exec it.
    pub fn from_staged_binary(staged: &Path, setup_command: &str) -> Self {
        match read_guest_agent_prefix(staged) {
            Ok(prefix) => match check_guest_agent(&prefix) {
                Ok(()) => Self::supported(),
                Err(rejection) => Self::unsupported(
                    format!(
                        "the staged guest terminal agent {} {}",
                        staged.display(),
                        rejection
                    ),
                    Some(setup_command),
                ),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::unsupported(
                format!(
                    "the guest terminal agent (temps-pty-agent) is not staged at {}",
                    staged.display()
                ),
                Some(setup_command),
            ),
            Err(e) => Self::unsupported(
                format!(
                    "the guest terminal agent at {} is unreadable: {}",
                    staged.display(),
                    e
                ),
                Some(setup_command),
            ),
        }
    }
}

/// Why a file cannot run as the guest terminal agent.
///
/// Guests are Linux on the host's CPU architecture and boot arbitrary user
/// images, so the agent has to be a static executable for that architecture:
/// anything else installs cleanly and then fails to exec inside the guest.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GuestAgentRejection {
    #[error("is not a Linux executable; the guest needs the static Linux (musl) build of temps-pty-agent")]
    NotElf,

    #[error(
        "is not a 64-bit little-endian ELF executable, which is what the guest architecture runs"
    )]
    UnsupportedElfClass,

    #[error(
        "is built for {found}, but sandboxes on this host run {expected}; \
         stage the {expected} static (musl) build of temps-pty-agent"
    )]
    WrongArchitecture {
        found: String,
        expected: &'static str,
    },

    #[error(
        "is dynamically linked (it asks for the loader {interpreter}), which a guest image \
         may not provide; stage the static (musl) build of temps-pty-agent"
    )]
    DynamicallyLinked { interpreter: String },

    #[error("has a malformed or truncated ELF program header table")]
    Malformed,
}

/// `e_machine` value for the guest architecture this host runs, or `None`
/// for a host architecture we have no expectation for (then the machine type
/// is not checked rather than guessed at).
fn expected_guest_machine() -> Option<(u16, &'static str)> {
    match std::env::consts::ARCH {
        "x86_64" => Some((62, "x86_64")),
        "aarch64" => Some((183, "aarch64")),
        _ => None,
    }
}

fn machine_name(machine: u16) -> String {
    match machine {
        3 => "i386".to_string(),
        40 => "arm".to_string(),
        62 => "x86_64".to_string(),
        183 => "aarch64".to_string(),
        243 => "riscv".to_string(),
        other => format!("ELF machine type {other}"),
    }
}

/// Bytes of a file's head read to validate it. The ELF header plus its
/// program-header table sit at the start of any real executable; this bounds
/// the read.
const GUEST_AGENT_PREFIX_BYTES: u64 = 64 * 1024;

fn read_guest_agent_prefix(path: &Path) -> std::io::Result<Vec<u8>> {
    use std::io::Read;

    let file = std::fs::File::open(path)?;
    let mut prefix = Vec::new();
    file.take(GUEST_AGENT_PREFIX_BYTES)
        .read_to_end(&mut prefix)?;
    Ok(prefix)
}

/// Check that `bytes` (the start of a candidate agent) is an executable the
/// guest can run: ELF, 64-bit little-endian, for this host's architecture,
/// with no program interpreter (a statically linked binary).
///
/// Only `PT_INTERP` is looked for, deliberately: a binary that names a
/// loader depends on the guest image shipping that exact loader, and the
/// guest is whatever image the user picked. musl static-pie builds have none.
pub fn check_guest_agent(bytes: &[u8]) -> Result<(), GuestAgentRejection> {
    const PT_INTERP: u32 = 3;
    const EI_CLASS_64: u8 = 2;
    const EI_DATA_LSB: u8 = 1;
    const MAX_PROGRAM_HEADERS: usize = 256;

    if !is_linux_elf(bytes) {
        return Err(GuestAgentRejection::NotElf);
    }
    // e_ident (16) + type/machine/version (8) + entry/phoff/shoff (24) +
    // flags/ehsize/phentsize/phnum (10) = 58 bytes of header we read.
    if bytes.len() < 58 {
        return Err(GuestAgentRejection::Malformed);
    }
    if bytes[4] != EI_CLASS_64 || bytes[5] != EI_DATA_LSB {
        return Err(GuestAgentRejection::UnsupportedElfClass);
    }
    let u16_at = |at: usize| u16::from_le_bytes([bytes[at], bytes[at + 1]]);
    let u32_at =
        |at: usize| u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
    let u64_at = |at: usize| {
        let mut raw = [0u8; 8];
        raw.copy_from_slice(&bytes[at..at + 8]);
        u64::from_le_bytes(raw)
    };

    let machine = u16_at(18);
    if let Some((expected_machine, expected)) = expected_guest_machine() {
        if machine != expected_machine {
            return Err(GuestAgentRejection::WrongArchitecture {
                found: machine_name(machine),
                expected,
            });
        }
    }

    let phoff = u64_at(32);
    let phentsize = usize::from(u16_at(54));
    let phnum = usize::from(u16_at(56));
    if phnum == 0 {
        return Ok(());
    }
    // An ELF64 program header is 56 bytes; the type is its first field.
    if phentsize < 56 || phnum > MAX_PROGRAM_HEADERS {
        return Err(GuestAgentRejection::Malformed);
    }
    let table_start = usize::try_from(phoff).map_err(|_| GuestAgentRejection::Malformed)?;
    let table_end = phnum
        .checked_mul(phentsize)
        .and_then(|len| table_start.checked_add(len))
        .ok_or(GuestAgentRejection::Malformed)?;
    if table_end > bytes.len() {
        return Err(GuestAgentRejection::Malformed);
    }
    for index in 0..phnum {
        let header = table_start + index * phentsize;
        if u32_at(header) != PT_INTERP {
            continue;
        }
        let offset = u64_at(header + 8);
        let size = u64_at(header + 32);
        let interpreter = usize::try_from(offset)
            .ok()
            .zip(usize::try_from(size).ok())
            .and_then(|(start, len)| bytes.get(start..start.checked_add(len)?))
            .map(|raw| {
                String::from_utf8_lossy(raw)
                    .trim_end_matches('\0')
                    .to_string()
            })
            .unwrap_or_else(|| "an unknown loader".to_string());
        return Err(GuestAgentRejection::DynamicallyLinked { interpreter });
    }
    Ok(())
}

/// Why a guest agent (`temps-pty-agent` or `temps-vm-agent`) could not be staged on the host.
#[derive(Debug, thiserror::Error)]
pub enum PtyAgentStagingError {
    #[error("guest agent {path} does not exist")]
    Missing { path: PathBuf },

    #[error("{path} {reason}")]
    Rejected {
        path: PathBuf,
        reason: GuestAgentRejection,
    },

    #[error("cannot stage guest agent {source_path} at {dest}: {reason}")]
    Io {
        source_path: PathBuf,
        dest: PathBuf,
        reason: String,
    },
}

/// Copy a guest agent (the terminal or the VM agent) from `source` to `dest` (mode 0755),
/// refusing anything the guest cannot run — not a Linux ELF, built for
/// another CPU, or dynamically linked — so a wrong build fails at setup
/// instead of at the first terminal (see [`check_guest_agent`]).
///
/// Both microVM backends' setup commands stage through this, so they agree on
/// what a valid agent is.
pub fn stage_pty_agent_binary(source: &Path, dest: &Path) -> Result<(), PtyAgentStagingError> {
    use std::io::{Read, Seek};
    use std::os::unix::fs::PermissionsExt;

    let io_err = |e: std::io::Error| PtyAgentStagingError::Io {
        source_path: source.to_path_buf(),
        dest: dest.to_path_buf(),
        reason: e.to_string(),
    };
    let mut file = match std::fs::File::open(source) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(PtyAgentStagingError::Missing {
                path: source.to_path_buf(),
            })
        }
        Err(e) => return Err(io_err(e)),
    };
    let mut prefix = Vec::new();
    std::io::Read::take(&mut file, GUEST_AGENT_PREFIX_BYTES)
        .read_to_end(&mut prefix)
        .map_err(io_err)?;
    // A file shorter than a header cannot be an ELF; check_guest_agent
    // reports that as "not a Linux executable" rather than as an I/O failure.
    check_guest_agent(&prefix).map_err(|reason| PtyAgentStagingError::Rejected {
        path: source.to_path_buf(),
        reason,
    })?;
    let parent = dest
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent).map_err(io_err)?;
    // Every writer owns a distinct file in the destination filesystem. Setup
    // and startup refresh may race, but only a complete agent is published.
    let mut staged = tempfile::Builder::new()
        .prefix(".temps-guest-agent-")
        .tempfile_in(parent)
        .map_err(io_err)?;
    file.rewind().map_err(io_err)?;
    std::io::copy(&mut file, staged.as_file_mut()).map_err(io_err)?;
    staged
        .as_file()
        .set_permissions(std::fs::Permissions::from_mode(0o755))
        .map_err(io_err)?;
    staged.as_file().sync_all().map_err(io_err)?;
    staged.persist(dest).map_err(|e| io_err(e.error))?;
    Ok(())
}

/// What [`sync_pty_agent_binary`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PtyAgentSync {
    /// The staged copy already matched the shipped one.
    AlreadyCurrent,
    /// The staged copy was missing or different and has been replaced.
    Staged,
}

/// Bring the agent staged at `dest` in line with the one shipped at `source`.
///
/// `temps upgrade` and the installer replace the `temps-pty-agent` that sits
/// beside the `temps` executable; the copy each microVM backend injects into
/// guests lives in the data directory and would otherwise only change when
/// someone re-ran a `setup` command. The server calls this at startup so a new
/// release's agent is the one guests get, with no operator step.
///
/// Compares content rather than timestamps (a rollback to an older release
/// must also replace a newer staged copy) and reuses
/// [`stage_pty_agent_binary`], so an unusable source is rejected and the
/// replacement is atomic.
pub fn sync_pty_agent_binary(
    source: &Path,
    dest: &Path,
) -> Result<PtyAgentSync, PtyAgentStagingError> {
    if files_have_same_content(source, dest) {
        return Ok(PtyAgentSync::AlreadyCurrent);
    }
    stage_pty_agent_binary(source, dest)?;
    Ok(PtyAgentSync::Staged)
}

/// Whether both files exist and hold identical bytes. Any read failure counts
/// as "different" so the caller falls through to staging, which reports the
/// real error with the paths attached.
fn files_have_same_content(a: &Path, b: &Path) -> bool {
    use std::io::Read;

    let (Ok(mut fa), Ok(mut fb)) = (std::fs::File::open(a), std::fs::File::open(b)) else {
        return false;
    };
    match (fa.metadata(), fb.metadata()) {
        (Ok(ma), Ok(mb)) if ma.len() == mb.len() => {}
        _ => return false,
    }
    let mut buf_a = [0u8; 64 * 1024];
    let mut buf_b = [0u8; 64 * 1024];
    loop {
        let Ok(n) = fa.read(&mut buf_a) else {
            return false;
        };
        if n == 0 {
            // Equal lengths were checked above, so `a` ending means `b` ends.
            return true;
        }
        if fb.read_exact(&mut buf_b[..n]).is_err() || buf_a[..n] != buf_b[..n] {
            return false;
        }
    }
}

/// Whether `header` (a file's leading bytes) starts an ELF image. The guests
/// are Linux, so an ELF header is the cheapest check that a staged agent is
/// not, say, a Mach-O build for the developer's laptop.
pub fn is_linux_elf(header: &[u8]) -> bool {
    header.starts_with(b"\x7fELF")
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

    /// Immutable image identity backing an existing sandbox, suitable for rollback.
    async fn image_identity(&self, handle: &SandboxHandle) -> Result<String, AgentError> {
        Err(AgentError::SandboxExecFailed {
            run_id: 0,
            sandbox_id: handle.sandbox_id.clone(),
            reason: format!(
                "provider '{}' cannot resolve immutable image identity",
                self.name()
            ),
        })
    }

    /// Probe the retained SDK protocol without creating a runtime or changing sandbox state.
    async fn check_agent_runtime(
        &self,
        _handle: &SandboxHandle,
    ) -> Result<RuntimeCompatibility, AgentError> {
        Ok(RuntimeCompatibility::Incompatible {
            reason: format!(
                "provider '{}' does not support the retained agent runtime",
                self.name()
            ),
        })
    }

    /// Confirm orphan harness termination after recovering a live sandbox.
    /// Implementations must not stop managed application processes.
    async fn recover_agent_harness(
        &self,
        handle: &SandboxHandle,
        _epoch: u64,
    ) -> Result<(), AgentError> {
        Err(AgentError::SandboxExecFailed {
            run_id: 0,
            sandbox_id: handle.sandbox_id.clone(),
            reason: format!(
                "provider '{}' requires a workspace runtime update for safe harness recovery",
                self.name()
            ),
        })
    }

    /// Return the base URL a process inside `handle` must use for its
    /// turn-scoped model relay. Backends with ordinary reachability can use
    /// the control-plane URL directly. Network-isolated backends override
    /// this with a capability-only bridge owned by that backend.
    async fn model_relay_base_url(
        &self,
        _handle: &SandboxHandle,
        control_plane_url: &str,
    ) -> Result<String, AgentError> {
        Ok(direct_model_relay_base_url(control_plane_url))
    }

    /// Return the base URL a process inside `handle` must use for bounded Git
    /// SmartHTTP access. Providers must explicitly opt in because this route
    /// can bridge an otherwise isolated sandbox to the control plane.
    async fn git_relay_base_url(
        &self,
        handle: &SandboxHandle,
        _control_plane_url: &str,
    ) -> Result<String, AgentError> {
        Err(AgentError::SandboxExecFailed {
            run_id: 0,
            sandbox_id: handle.sandbox_id.clone(),
            reason: format!(
                "Git relay is not supported by sandbox provider '{}'",
                self.name()
            ),
        })
    }

    /// Return the URL a process inside `handle` must use for a turn-scoped
    /// harness MCP server. Backends with ordinary control-plane reachability
    /// can use the registered URL directly. Network-isolated backends override
    /// this with a capability-only relay owned by that backend.
    async fn harness_mcp_url(
        &self,
        _handle: &SandboxHandle,
        _control_plane_url: &str,
        registered_url: &str,
    ) -> Result<String, AgentError> {
        Ok(registered_url.to_string())
    }

    /// Attach a sandbox and a bounded set of application-owned data services
    /// to an isolated per-application network. The default rejects the
    /// operation; only providers capable of enforcing network membership
    /// should implement it.
    async fn configure_application_network(
        &self,
        handle: &SandboxHandle,
        network_name: &str,
        service_containers: &[String],
    ) -> Result<(), AgentError> {
        let _ = (network_name, service_containers);
        Err(AgentError::SandboxExecFailed {
            run_id: 0,
            sandbox_id: handle.sandbox_id.clone(),
            reason: format!(
                "application networks are not supported by sandbox provider '{}'",
                self.name()
            ),
        })
    }

    /// Open the private retained-agent daemon protocol, without a TTY or shell.
    /// The caller must authorize this sandbox before opening the connection.
    async fn connect_agent_runtime(
        &self,
        handle: &SandboxHandle,
    ) -> Result<PtyAttachment, AgentError> {
        Err(AgentError::SandboxExecFailed {
            run_id: 0,
            sandbox_id: handle.sandbox_id.clone(),
            reason: format!(
                "retained agent runtime transport is unavailable for provider '{}'",
                self.name()
            ),
        })
    }

    /// A byte pipe to TCP `port` on the sandbox's own loopback, carrying
    /// whatever protocol the process listening there speaks. The ingress
    /// uses it for preview URLs of sandboxes it cannot dial directly, i.e.
    /// those on worker nodes. The caller must authorize the request first.
    async fn connect_port(
        &self,
        handle: &SandboxHandle,
        port: u16,
    ) -> Result<PtyAttachment, AgentError> {
        Err(AgentError::SandboxExecFailed {
            run_id: 0,
            sandbox_id: handle.sandbox_id.clone(),
            reason: format!(
                "port {} of sandbox '{}' cannot be reached through provider '{}'",
                port,
                handle.sandbox_name,
                self.name()
            ),
        })
    }

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

    /// [`read_file`](Self::read_file), refusing files larger than
    /// `max_bytes` with [`AgentError::Validation`] naming the sandbox, the
    /// path and the limit.
    ///
    /// Used where the caller must bound its memory (a worker node answering
    /// the control plane, ADR-048). The default checks after reading;
    /// providers that can stop early (Docker) override it so an oversized
    /// file is never buffered.
    async fn read_file_bounded(
        &self,
        handle: &SandboxHandle,
        path: &str,
        max_bytes: u64,
    ) -> Result<Vec<u8>, AgentError> {
        let contents = self.read_file(handle, path).await?;
        if contents.len() as u64 > max_bytes {
            return Err(file_too_large(handle, path, max_bytes));
        }
        Ok(contents)
    }

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

    /// Bring the sandbox's work dir back into `host_dir`, the host checkout
    /// it was created from, so callers that read a run's results from the
    /// host (change detection, PR file collection) see the agent's edits.
    ///
    /// The default does nothing: it is correct for backends whose work dir
    /// *is* `host_dir` (Docker bind-mounts it). Backends that copy the
    /// workspace into a guest override it.
    async fn sync_workspace_to_host(
        &self,
        handle: &SandboxHandle,
        host_dir: &std::path::Path,
    ) -> Result<(), AgentError> {
        let _ = (handle, host_dir);
        Ok(())
    }

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

    /// Strictly fence matching process trees before adopting compute from a
    /// previous control-plane generation.
    ///
    /// Unlike [`SandboxProvider::kill_processes`], this operation is not
    /// best-effort. It freezes matching roots, discovers and freezes their
    /// descendants, sends SIGTERM, escalates survivors to SIGKILL, and only
    /// succeeds after every recorded PID is absent. Provider exec failures,
    /// timeouts, and surviving processes are errors so callers can fail
    /// closed instead of running a second harness in the same workspace.
    async fn fence_process_trees(
        &self,
        handle: &SandboxHandle,
        patterns: &[&str],
    ) -> Result<(), AgentError> {
        const FENCE_SCRIPT: &str = r#"
set -eu

roots=""
self_pid=$$
for pattern in "$@"; do
  set +e
  matches=$(pgrep -f "$pattern" 2>/dev/null)
  match_status=$?
  set -e
  [ "$match_status" -le 1 ] || exit 71
  for pid in $matches; do
    [ "$pid" = "$self_pid" ] && continue
    case " $roots " in
      *" $pid "*) ;;
      *) roots="$roots $pid" ;;
    esac
  done
done

[ -n "$roots" ] || exit 0

# Freeze roots before walking the tree so a stale harness cannot keep
# spawning children while the fence is being established.
kill -STOP $roots 2>/dev/null || true
targets="$roots"
frontier="$roots"
while [ -n "$frontier" ]; do
  next=""
  process_table=$(ps -eo pid=,ppid=)
  for parent in $frontier; do
    for child in $(printf '%s\n' "$process_table" | awk -v parent="$parent" '$2 == parent { print $1 }'); do
      case " $targets " in
        *" $child "*) ;;
        *)
          kill -STOP "$child" 2>/dev/null || true
          targets="$targets $child"
          next="$next $child"
          ;;
      esac
    done
  done
  frontier="$next"
done

# SIGTERM is queued while the processes are stopped. SIGCONT lets graceful
# handlers run; processes that ignore TERM are killed after the deadline.
kill -TERM $targets 2>/dev/null || true
kill -CONT $targets 2>/dev/null || true

is_running() {
  state=$(ps -o stat= -p "$1" 2>/dev/null || true)
  case "$state" in
    ""|Z*) return 1 ;;
    *) return 0 ;;
  esac
}

attempt=0
remaining="$targets"
while [ "$attempt" -lt 30 ]; do
  remaining=""
  for pid in $targets; do
    if is_running "$pid"; then
      remaining="$remaining $pid"
    fi
  done
  [ -n "$remaining" ] || exit 0
  attempt=$((attempt + 1))
  sleep 0.1
done

kill -KILL $remaining 2>/dev/null || true
sleep 0.1
for pid in $remaining; do
  if is_running "$pid"; then
    exit 70
  fi
done
"#;

        let mut command = vec![
            "sh".to_string(),
            "-c".to_string(),
            FENCE_SCRIPT.to_string(),
            "temps-process-fence".to_string(),
        ];
        command.extend(patterns.iter().map(|pattern| (*pattern).to_string()));

        let result = tokio::time::timeout(
            Duration::from_secs(10),
            self.exec(handle, command, HashMap::new(), None),
        )
        .await
        .map_err(|_| AgentError::SandboxExecFailed {
            run_id: 0,
            sandbox_id: handle.sandbox_id.clone(),
            reason: "verified process fencing timed out".to_string(),
        })??;

        if result.exit_code != 0 {
            return Err(AgentError::SandboxExecFailed {
                run_id: 0,
                sandbox_id: handle.sandbox_id.clone(),
                reason: format!(
                    "verified process fencing failed with exit code {}",
                    result.exit_code
                ),
            });
        }

        Ok(())
    }

    /// Destroy sandbox and clean up its container.
    ///
    /// When `purge_volumes` is true, the provider also removes any per-run
    /// persistent volumes (e.g. the `/home/temps` named volume for Docker).
    /// When false, volumes are left in place so a subsequent `create` with
    /// the same run_id resumes the previous home directory — the workspace
    /// uses this to preserve Claude auth / shell history across a session
    /// close+reopen cycle.
    async fn destroy(&self, handle: &SandboxHandle, purge_volumes: bool) -> Result<(), AgentError>;

    // NOTE: there is deliberately no background "reclaim orphaned storage"
    // hook here. Sandbox storage is freed only by an explicit destroy,
    // which knows exactly which volume belongs to the sandbox being
    // deleted. A sweep has to *infer* that instead, and every predicate we
    // tried infers it wrong somewhere: "no container references it" also
    // describes a sandbox mid-recreate whose image is still pulling, and
    // "no database row claims it" also describes every sandbox belonging to
    // a second temps instance sharing the same Docker daemon, or to a
    // database that was restored, swapped, or cascade-deleted. Getting it
    // wrong destroys a user's `/home/temps` — Claude credentials, shell
    // history, project state — silently and irreversibly.
    //
    // Volumes stranded by older builds are reclaimed by the operator with
    // `docker volume prune --filter label=sh.temps.sandbox.home`, which is
    // explicit, reviewable, and cannot run while they aren't looking.

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

    /// Replace the egress policy of an existing sandbox, running or stopped.
    /// `policy` is normalized. The new policy holds for every connection
    /// opened after this returns; connections already open may be cut.
    /// Default: the provider cannot enforce allow-lists, so `internet` (what
    /// it already does) is accepted and `restricted` is refused with a typed
    /// error.
    async fn set_egress_policy(
        &self,
        handle: &SandboxHandle,
        policy: &SandboxEgressPolicy,
    ) -> Result<(), AgentError> {
        if policy.is_restricted() {
            return Err(AgentError::SandboxEgressPolicyUnsupported {
                sandbox: handle.sandbox_name.clone(),
                backend: handle.backend.to_string(),
            });
        }
        Ok(())
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

    /// Recover a sandbox by container name on a specific node (ADR-048).
    /// `None` = the local host. Single-host providers only know the local
    /// host, so the default ignores `node_id`;
    /// [`node_routing::NodeRoutingSandboxProvider`] overrides it to ask the
    /// owning worker.
    async fn recover_by_name_on(
        &self,
        node_id: Option<i32>,
        container_name: &str,
    ) -> Result<Option<SandboxHandle>, AgentError> {
        let _ = node_id;
        self.recover_by_name(container_name).await
    }

    /// Release what a sandbox whose container is already gone may have left
    /// behind (for Docker: its isolated network and egress proxy, which pin
    /// a subnet from the daemon's finite address pool). Called when a
    /// sandbox is destroyed and no container could be recovered for it, so
    /// the normal `destroy` path never ran. Idempotent. Default: nothing to
    /// release.
    async fn release_orphaned_resources(&self, container_name: &str) -> Result<(), AgentError> {
        let _ = container_name;
        Ok(())
    }

    /// [`release_orphaned_resources`](Self::release_orphaned_resources) on a
    /// specific node (ADR-048). `None` = the local host.
    async fn release_orphaned_resources_on(
        &self,
        node_id: Option<i32>,
        container_name: &str,
    ) -> Result<(), AgentError> {
        let _ = node_id;
        self.release_orphaned_resources(container_name).await
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

    /// Open a bidirectional channel to the sandbox's PTY agent (ADR-008).
    ///
    /// Powers interactive terminals: the caller speaks the agent's framed
    /// protocol over the returned stream to open a tab, send keystrokes,
    /// forward resizes, and receive output.
    ///
    /// Default: unsupported. Backends that can't reach an in-sandbox unix
    /// socket say so explicitly rather than returning an empty stream that
    /// looks like a terminal which simply never echoes — a user facing that
    /// has no way to tell "not implemented" from "my shell is broken".
    async fn attach_pty(&self, handle: &SandboxHandle) -> Result<PtyAttachment, AgentError> {
        let _ = handle;
        Err(AgentError::SandboxExecFailed {
            run_id: 0,
            sandbox_id: String::new(),
            reason: format!(
                "interactive terminals are not supported by sandbox provider '{}'",
                self.name()
            ),
        })
    }

    /// Capture the current state of `handle` as a reusable [`SnapshotArtifact`].
    ///
    /// The sandbox should be stopped before calling this (the service layer is
    /// responsible for quiescing). For Docker, this executes the three-step
    /// credential-scrubbing protocol from ADR-037 §4 before committing:
    ///
    /// 1. Shred `/etc/temps/credential-daemon.env` inside the container.
    /// 2. Strip injected env vars from the committed image config.
    /// 3. Reject the snapshot if any known-sensitive key pattern survives.
    ///
    /// `label` is a human-readable annotation stored on the DB row; it is
    /// **not** used as part of the content-addressed file name.
    /// `max_size_bytes` is a hard cap for all artifacts produced by this
    /// operation; providers must abort and clean up before crossing it.
    ///
    /// Docker and Firecracker override this method. Providers without a
    /// persistent snapshot format leave the default implementation in place.
    async fn take_snapshot(
        &self,
        handle: &SandboxHandle,
        label: Option<String>,
        max_size_bytes: u64,
    ) -> Result<SnapshotArtifact, AgentError> {
        let _ = label;
        let _ = max_size_bytes;
        Err(AgentError::SandboxExecFailed {
            run_id: 0,
            sandbox_id: handle.sandbox_id.clone(),
            reason: format!(
                "take_snapshot is not supported by sandbox provider '{}'",
                self.name()
            ),
        })
    }

    /// Create and start a new sandbox seeded from a previously-taken
    /// [`SnapshotArtifact`] instead of a base image.
    ///
    /// For Docker: ensures the snapshot image tag is present in the daemon
    /// (loading from the tarball if absent), then passes `artifact.image_ref`
    /// as the `image` in the container create config. For Firecracker: clones
    /// the captured sparse root disk into a new VM and boots it.
    ///
    /// The caller is responsible for verifying that `artifact.backend` matches
    /// the target provider's backend before calling — the service layer does
    /// this check and returns 422 on mismatch.
    ///
    /// **Default: returns `AgentError::SandboxExecFailed` ("not supported").**
    async fn create_from_snapshot(
        &self,
        artifact: &SnapshotArtifact,
        config: SandboxCreateConfig,
    ) -> Result<SandboxHandle, AgentError> {
        let _ = artifact;
        let _ = config;
        Err(AgentError::SandboxExecFailed {
            run_id: 0,
            sandbox_id: String::new(),
            reason: format!(
                "create_from_snapshot is not supported by sandbox provider '{}'",
                self.name()
            ),
        })
    }

    /// Remove a snapshot image from the backend's image store.
    ///
    /// Called by `SnapshotService::delete_snapshot` after the DB row is
    /// soft-deleted. Failure is logged but does not fail the delete — the
    /// tarball (the source of truth) is already removed; an orphaned image
    /// tag is cleaned up on the next GC or by the operator.
    ///
    /// **Default: a no-op with a debug log.** Backends other than Docker (e.g.
    /// Local, Firecracker) have no image store concept and leave this default.
    async fn delete_image(&self, image_ref: &str) -> Result<(), AgentError> {
        tracing::debug!(
            image_ref = %image_ref,
            provider = %self.name(),
            "delete_image: provider does not support image deletion (no-op)"
        );
        Ok(())
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

/// Builders for ELF images that exercise [`check_guest_agent`], shared by the
/// backends' tests so they all agree on what "a usable agent" looks like.
#[cfg(test)]
pub(crate) mod test_elf {
    /// `e_machine` of the architecture tests run on, which is what the
    /// guest check expects.
    pub(crate) fn host_machine() -> u16 {
        match std::env::consts::ARCH {
            "aarch64" => 183,
            _ => 62,
        }
    }

    /// A minimal 64-bit little-endian ELF: header, one program header, then
    /// `payload`. With `interpreter` the program header is `PT_INTERP` and the
    /// loader path follows it; otherwise it is a plain `PT_LOAD` (static).
    pub(crate) fn elf(machine: u16, interpreter: Option<&str>, payload: &[u8]) -> Vec<u8> {
        let mut out = vec![0u8; 64 + 56];
        out[0..8].copy_from_slice(b"\x7fELF\x02\x01\x01\x00");
        out[16..18].copy_from_slice(&2u16.to_le_bytes()); // ET_EXEC
        out[18..20].copy_from_slice(&machine.to_le_bytes());
        out[20..24].copy_from_slice(&1u32.to_le_bytes());
        out[32..40].copy_from_slice(&64u64.to_le_bytes()); // e_phoff
        out[52..54].copy_from_slice(&64u16.to_le_bytes()); // e_ehsize
        out[54..56].copy_from_slice(&56u16.to_le_bytes()); // e_phentsize
        out[56..58].copy_from_slice(&1u16.to_le_bytes()); // e_phnum
        match interpreter {
            Some(path) => {
                let bytes = path.as_bytes();
                out[64..68].copy_from_slice(&3u32.to_le_bytes()); // PT_INTERP
                out[72..80].copy_from_slice(&120u64.to_le_bytes()); // p_offset
                out[96..104].copy_from_slice(&((bytes.len() + 1) as u64).to_le_bytes());
                out.extend_from_slice(bytes);
                out.push(0);
            }
            None => out[64..68].copy_from_slice(&1u32.to_le_bytes()), // PT_LOAD
        }
        out.extend_from_slice(payload);
        out
    }

    /// A static executable for this host: what `temps-pty-agent` is.
    pub(crate) fn static_for_host(payload: &[u8]) -> Vec<u8> {
        elf(host_machine(), None, payload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio::process::Command;

    #[test]
    fn staged_agent_capability_distinguishes_missing_foreign_and_ready() {
        let dir = tempfile::tempdir().unwrap();
        let staged = dir.path().join(PTY_AGENT_BINARY_NAME);
        let cmd = "temps example setup";

        let missing = TerminalCapability::from_staged_binary(&staged, cmd);
        assert!(!missing.supported);
        let reason = missing.reason.unwrap();
        assert!(reason.contains("not staged"), "{reason}");
        assert!(reason.contains(&staged.display().to_string()), "{reason}");
        assert_eq!(missing.setup_command.as_deref(), Some(cmd));

        // A Mach-O header (macOS build) must not be mistaken for a guest binary.
        std::fs::write(&staged, [0xcf, 0xfa, 0xed, 0xfe, 0, 0, 0, 0]).unwrap();
        let foreign = TerminalCapability::from_staged_binary(&staged, cmd);
        assert!(!foreign.supported);
        assert!(foreign.reason.unwrap().contains("not a Linux executable"));

        std::fs::write(&staged, test_elf::static_for_host(b"")).unwrap();
        assert_eq!(
            TerminalCapability::from_staged_binary(&staged, cmd),
            TerminalCapability::supported()
        );

        // A valid Linux ELF that the guest still cannot run is not "ready".
        let other_machine = if test_elf::host_machine() == 62 {
            183
        } else {
            62
        };
        std::fs::write(&staged, test_elf::elf(other_machine, None, b"")).unwrap();
        let wrong_arch = TerminalCapability::from_staged_binary(&staged, cmd);
        assert!(!wrong_arch.supported);
        assert_eq!(wrong_arch.setup_command.as_deref(), Some(cmd));
        let reason = wrong_arch.reason.unwrap();
        assert!(reason.contains("sandboxes on this host run"), "{reason}");

        std::fs::write(
            &staged,
            test_elf::elf(
                test_elf::host_machine(),
                Some("/lib64/ld-linux-x86-64.so.2"),
                b"",
            ),
        )
        .unwrap();
        let dynamic = TerminalCapability::from_staged_binary(&staged, cmd);
        assert!(!dynamic.supported);
        assert!(dynamic.reason.unwrap().contains("dynamically linked"));

        // Too short to hold a header: unreadable, not "ready".
        std::fs::write(&staged, b"\x7f").unwrap();
        assert!(!TerminalCapability::from_staged_binary(&staged, cmd).supported);
    }

    #[test]
    fn guest_agent_check_distinguishes_static_dynamic_and_malformed() {
        let host = test_elf::host_machine();
        assert_eq!(check_guest_agent(&test_elf::static_for_host(b"x")), Ok(()));

        assert_eq!(check_guest_agent(b""), Err(GuestAgentRejection::NotElf));
        assert_eq!(
            check_guest_agent(b"MZ\x90\x00 not an elf at all"),
            Err(GuestAgentRejection::NotElf)
        );

        // 32-bit and big-endian images are rejected before any field is read.
        let mut class32 = test_elf::static_for_host(b"");
        class32[4] = 1;
        assert_eq!(
            check_guest_agent(&class32),
            Err(GuestAgentRejection::UnsupportedElfClass)
        );
        let mut big_endian = test_elf::static_for_host(b"");
        big_endian[5] = 2;
        assert_eq!(
            check_guest_agent(&big_endian),
            Err(GuestAgentRejection::UnsupportedElfClass)
        );

        // The loader path is reported, so the message can name it.
        assert_eq!(
            check_guest_agent(&test_elf::elf(host, Some("/lib/ld-musl-x86_64.so.1"), b"")),
            Err(GuestAgentRejection::DynamicallyLinked {
                interpreter: "/lib/ld-musl-x86_64.so.1".to_string()
            })
        );

        // A header table pointing past what was read, or an absurd count.
        let mut truncated = test_elf::static_for_host(b"");
        truncated.truncate(80);
        assert_eq!(
            check_guest_agent(&truncated),
            Err(GuestAgentRejection::Malformed)
        );
        let mut huge = test_elf::static_for_host(b"");
        huge[56..58].copy_from_slice(&u16::MAX.to_le_bytes());
        assert_eq!(
            check_guest_agent(&huge),
            Err(GuestAgentRejection::Malformed)
        );
        let mut far = test_elf::static_for_host(b"");
        far[32..40].copy_from_slice(&u64::MAX.to_le_bytes());
        assert_eq!(check_guest_agent(&far), Err(GuestAgentRejection::Malformed));
    }

    #[test]
    fn staging_installs_an_elf_and_rejects_everything_else() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("guest").join(PTY_AGENT_BINARY_NAME);

        let missing = stage_pty_agent_binary(&dir.path().join("absent"), &dest).unwrap_err();
        assert!(matches!(missing, PtyAgentStagingError::Missing { .. }));
        assert!(missing.to_string().contains("absent"));
        assert!(!dest.exists(), "a failed stage must not leave a file");

        let foreign = dir.path().join("macho");
        std::fs::write(&foreign, [0xcf, 0xfa, 0xed, 0xfe, 1, 2, 3, 4]).unwrap();
        let err = stage_pty_agent_binary(&foreign, &dest).unwrap_err();
        assert!(matches!(
            err,
            PtyAgentStagingError::Rejected {
                reason: GuestAgentRejection::NotElf,
                ..
            }
        ));
        assert!(!dest.exists());

        // Right format, wrong CPU or needing a loader: refused at setup, not
        // discovered as an exec failure inside the first guest.
        let wrong_cpu = dir.path().join("wrong-cpu");
        let other_machine = if test_elf::host_machine() == 62 {
            183
        } else {
            62
        };
        std::fs::write(&wrong_cpu, test_elf::elf(other_machine, None, b"")).unwrap();
        assert!(matches!(
            stage_pty_agent_binary(&wrong_cpu, &dest).unwrap_err(),
            PtyAgentStagingError::Rejected {
                reason: GuestAgentRejection::WrongArchitecture { .. },
                ..
            }
        ));
        let glibc = dir.path().join("glibc");
        std::fs::write(
            &glibc,
            test_elf::elf(
                test_elf::host_machine(),
                Some("/lib64/ld-linux-x86-64.so.2"),
                b"",
            ),
        )
        .unwrap();
        let err = stage_pty_agent_binary(&glibc, &dest).unwrap_err();
        assert!(err.to_string().contains("ld-linux"), "{err}");
        assert!(!dest.exists());

        let tiny = dir.path().join("tiny");
        std::fs::write(&tiny, b"\x7f").unwrap();
        assert!(matches!(
            stage_pty_agent_binary(&tiny, &dest).unwrap_err(),
            PtyAgentStagingError::Rejected {
                reason: GuestAgentRejection::NotElf,
                ..
            }
        ));

        let elf = dir.path().join("agent");
        let payload = test_elf::static_for_host(b"payload");
        std::fs::write(&elf, &payload).unwrap();
        std::fs::set_permissions(&elf, std::fs::Permissions::from_mode(0o600)).unwrap();
        stage_pty_agent_binary(&elf, &dest).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), payload);
        assert_eq!(
            std::fs::metadata(&dest).unwrap().permissions().mode() & 0o777,
            0o755,
            "the staged agent must be executable whatever the source mode"
        );
        // No temporary left behind, and re-staging over an existing file works.
        assert!(!dest.with_extension("staging").exists());
        stage_pty_agent_binary(&elf, &dest).unwrap();
    }

    #[test]
    fn overlapping_agent_writes_publish_only_complete_binaries() {
        use std::sync::{Arc, Barrier};
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("agent");
        let payloads = [
            test_elf::static_for_host(&vec![1; 2 * 1024 * 1024]),
            test_elf::static_for_host(&vec![2; 2 * 1024 * 1024]),
        ];
        let sources = [dir.path().join("source-a"), dir.path().join("source-b")];
        for (source, bytes) in sources.iter().zip(&payloads) {
            std::fs::write(source, bytes).unwrap();
        }
        stage_pty_agent_binary(&sources[0], &dest).unwrap();
        let gate = Arc::new(Barrier::new(3));
        std::thread::scope(|scope| {
            for source in &sources {
                let gate = gate.clone();
                let dest = &dest;
                scope.spawn(move || {
                    gate.wait();
                    for _ in 0..12 {
                        stage_pty_agent_binary(source, dest).unwrap();
                    }
                });
            }
            gate.wait();
            for _ in 0..48 {
                let published = std::fs::read(&dest).unwrap();
                assert!(
                    payloads.contains(&published),
                    "published a partial or mixed binary"
                );
            }
        });
        assert!(payloads.contains(&std::fs::read(&dest).unwrap()));
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            3,
            "temporary files leaked"
        );
    }

    #[test]
    fn sync_replaces_a_stale_agent_and_leaves_a_current_one_alone() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("shipped");
        let dest = dir.path().join("guest").join(PTY_AGENT_BINARY_NAME);

        std::fs::write(&source, test_elf::static_for_host(b"v1")).unwrap();
        assert_eq!(
            sync_pty_agent_binary(&source, &dest).unwrap(),
            PtyAgentSync::Staged,
            "a missing staged copy is created"
        );
        assert_eq!(
            sync_pty_agent_binary(&source, &dest).unwrap(),
            PtyAgentSync::AlreadyCurrent
        );

        // Same length, different bytes: a release that only changed content.
        std::fs::write(&source, test_elf::static_for_host(b"v2")).unwrap();
        assert_eq!(
            sync_pty_agent_binary(&source, &dest).unwrap(),
            PtyAgentSync::Staged
        );
        assert_eq!(
            std::fs::read(&dest).unwrap(),
            test_elf::static_for_host(b"v2")
        );

        // Different length, e.g. a downgrade to a smaller build.
        std::fs::write(&source, test_elf::static_for_host(b"v3-longer")).unwrap();
        assert_eq!(
            sync_pty_agent_binary(&source, &dest).unwrap(),
            PtyAgentSync::Staged
        );
        assert!(!dest.with_extension("staging").exists());
    }

    #[test]
    fn sync_never_replaces_a_working_agent_with_an_unusable_one() {
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("good");
        let dest = dir.path().join(PTY_AGENT_BINARY_NAME);
        let good_bytes = test_elf::static_for_host(b"good");
        std::fs::write(&good, &good_bytes).unwrap();
        sync_pty_agent_binary(&good, &dest).unwrap();

        // The macOS build next to a development `temps`.
        let foreign = dir.path().join("foreign");
        std::fs::write(&foreign, [0xcf, 0xfa, 0xed, 0xfe, 0, 0, 0, 0]).unwrap();
        assert!(matches!(
            sync_pty_agent_binary(&foreign, &dest).unwrap_err(),
            PtyAgentStagingError::Rejected { .. }
        ));
        assert_eq!(std::fs::read(&dest).unwrap(), good_bytes);

        assert!(matches!(
            sync_pty_agent_binary(&dir.path().join("absent"), &dest).unwrap_err(),
            PtyAgentStagingError::Missing { .. }
        ));
        assert_eq!(std::fs::read(&dest).unwrap(), good_bytes);
    }

    #[test]
    fn vm_agent_and_docker_agree_on_the_pty_socket_path() {
        // The microVM guests run the same agent as the Docker sandboxes and
        // the host handler dials one fixed path; a drift would silently
        // break terminals on one backend only.
        assert_eq!(temps_vm_agent::PTY_AGENT_SOCKET, PTY_AGENT_SOCKET);
    }

    #[test]
    fn sandbox_backend_round_trips_through_its_string_form() {
        for backend in [
            SandboxBackend::Docker,
            SandboxBackend::Firecracker,
            SandboxBackend::Microsandbox,
            SandboxBackend::Local,
        ] {
            let text = backend.to_string();
            assert_eq!(text.parse::<SandboxBackend>().unwrap(), backend);
            // serde and Display/FromStr must agree: the API persists one
            // and parses the other.
            assert_eq!(
                serde_json::to_value(backend).unwrap(),
                serde_json::Value::String(text)
            );
        }
        assert_eq!(
            "microsandbox".parse::<SandboxBackend>().unwrap(),
            SandboxBackend::Microsandbox
        );
        let err = "libkrun".parse::<SandboxBackend>().unwrap_err();
        assert!(err.contains("libkrun"), "{err}");
    }

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

    #[test]
    fn direct_model_relay_base_includes_the_registered_api_route() {
        assert_eq!(
            direct_model_relay_base_url("http://control-plane.test:8080/"),
            "http://control-plane.test:8080/api/ai/sandbox-models"
        );
    }

    #[tokio::test]
    async fn strict_fence_kills_a_term_resistant_process_tree() {
        let marker = format!("temps-fence-test-{}", std::process::id());
        let child_marker = format!("{marker}-child");
        let root_marker = format!("{marker}-root");
        let script = format!(
            "trap '' TERM; sh -c 'trap \"\" TERM; while :; do sleep 1; done' {child_marker} & echo $!; while :; do sleep 1; done"
        );
        let mut root = Command::new("sh")
            .arg("-c")
            .arg(script)
            .arg(&root_marker)
            .stdout(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("spawn TERM-resistant process tree");
        let root_pid = root.id().expect("root process id");
        let child_pid: u32 = BufReader::new(root.stdout.take().expect("child pid pipe"))
            .lines()
            .next_line()
            .await
            .expect("read child pid")
            .expect("child pid line")
            .parse()
            .expect("numeric child pid");

        let probe = Command::new("pgrep")
            .args(["-f", &format!("{root_marker}$")])
            .output()
            .await
            .expect("run process discovery probe");
        if !probe.status.success() {
            unsafe {
                libc::kill(root_pid as i32, libc::SIGKILL);
                libc::kill(child_pid as i32, libc::SIGKILL);
            }
            let _ = root.wait().await;
            eprintln!("skipping strict fence process-tree test: process discovery unavailable");
            return;
        }

        let handle = SandboxHandle {
            node_id: None,
            sandbox_id: marker.clone(),
            sandbox_name: marker.clone(),
            work_dir: std::env::current_dir().expect("current directory"),
            backend: SandboxBackend::Local,
            image: String::new(),
            conditions: Vec::new(),
        };
        let provider = local::LocalSandboxProvider::new();
        let fence_result = provider
            .fence_process_trees(&handle, &[&format!("{root_marker}$")])
            .await;

        if fence_result.is_err() {
            // Test cleanup must not leave an intentionally TERM-resistant
            // process behind when an assertion fails.
            unsafe {
                libc::kill(root_pid as i32, libc::SIGKILL);
                libc::kill(child_pid as i32, libc::SIGKILL);
            }
        }

        let root_wait = tokio::time::timeout(Duration::from_secs(1), root.wait()).await;
        if root_wait.is_err() {
            unsafe {
                libc::kill(root_pid as i32, libc::SIGKILL);
                libc::kill(child_pid as i32, libc::SIGKILL);
            }
        }
        let root_status = match root_wait {
            Ok(status) => status.expect("wait for root process"),
            Err(error) => {
                let _ = root.wait().await;
                panic!("root process must exit: {error}")
            }
        };
        fence_result.expect("verified fence must escalate TERM-resistant processes");
        assert!(!root_status.success());
        let child_state = Command::new("ps")
            .args(["-o", "stat=", "-p", &child_pid.to_string()])
            .output()
            .await
            .expect("inspect child state");
        let state = String::from_utf8_lossy(&child_state.stdout);
        assert!(
            state.trim().is_empty() || state.trim_start().starts_with('Z'),
            "child process remained active with state {state:?}"
        );
    }
}
