// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The SDK-backed microsandbox provider (ADR-050). Compiled for every
//! target except musl; see the parent module.

use async_trait::async_trait;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ::microsandbox::sandbox::{
    DeploymentProfile, FsEntryKind, FsSetAttrs, RlimitResource, SandboxHandle as MsbSandboxHandle,
    SandboxStatus, SecurityProfile,
};
use ::microsandbox::setup::{InstallOptions, ResolvedRuntime};
use ::microsandbox::{
    ExecEvent, LocalBackend, MicrosandboxError, NetworkPolicy, NetworkProfile, Sandbox,
};

use super::{
    host_virtualization, MicrosandboxSandboxConfig, MicrosandboxUnavailable, MICROSANDBOX_VERSION,
    MSB_SANDBOX_NAME_PREFIX, PROVIDER_NAME, WORK_DIR,
};
use crate::ai_cli::OnEventCallback;
use crate::error::AgentError;
use crate::sandbox::{
    ExecStream, KillSignal, OnStreamEventCallback, SandboxBackend, SandboxCreateConfig,
    SandboxExecResult, SandboxHandle, SandboxProvider,
};

/// Image used when a sandbox doesn't specify one. Same default as the
/// Firecracker backend so the two are interchangeable for callers.
const DEFAULT_IMAGE: &str = "alpine:3.20";

/// Smallest guest memory accepted. Below this the guest kernel and agent do
/// not reliably boot.
const MIN_MEMORY_MIB: u64 = 128;

/// Upper bound on captured output per stream for one exec. Output past this
/// is still streamed to callbacks but not retained, so a chatty command can't
/// exhaust host memory.
const MAX_CAPTURED_OUTPUT_BYTES: usize = 16 * 1024 * 1024;

/// Longest line delivered to output callbacks. Callers parse each line as
/// one record (agent CLIs emit one JSON event per line, carrying whole
/// messages and tool inputs), so a line is never split: one longer than
/// this is dropped from the callbacks as a whole and reported in the exec
/// result. Bounds host memory per stream however the guest writes.
const MAX_LINE_BYTES: usize = 8 * 1024 * 1024;

/// Chunk size for streaming host files into a guest.
const UPLOAD_CHUNK_BYTES: usize = 256 * 1024;

/// How long `connect` waits for the in-guest agent handshake.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// Network policy a sandbox gets for a given `network_mode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NetworkPlan {
    /// `"none"`: no network interface at all.
    Disabled,
    /// `"full"` (default): public internet only. Private/LAN ranges, the
    /// host, loopback, link-local and cloud metadata are denied by the
    /// host-side policy engine.
    PublicOnly,
    /// `"restricted"` ("Temps network only"): DNS plus the sandbox host.
    /// Everything else is denied host-side.
    HostOnly,
}

impl NetworkPlan {
    fn from_mode(mode: Option<&str>) -> Result<Self, AgentError> {
        match mode {
            None | Some("full") | Some("open") => Ok(Self::PublicOnly),
            Some("restricted") => Ok(Self::HostOnly),
            Some("none") => Ok(Self::Disabled),
            Some(other) => Err(AgentError::Validation {
                message: format!(
                    "network_mode '{}' is not supported by the {} backend \
                     (expected \"full\", \"restricted\" or \"none\")",
                    other, PROVIDER_NAME
                ),
            }),
        }
    }

    fn policy(self) -> Option<NetworkPolicy> {
        match self {
            Self::Disabled => None,
            Self::PublicOnly => Some(NetworkPolicy::from_profiles([NetworkProfile::Public])),
            Self::HostOnly => Some(NetworkPolicy::from_profiles([NetworkProfile::Host])),
        }
    }
}

/// Resolved VM shape for one create.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ResourcePlan {
    cpus: u8,
    memory_mib: u32,
    disk_mib: Option<u32>,
    nproc: Option<u64>,
}

impl MicrosandboxSandboxConfig {
    /// Temps-owned SDK config file. Pinned so a user-level
    /// `~/.microsandbox/config.json` can't change what Temps launches.
    fn config_path(&self) -> PathBuf {
        self.home().join("config.json")
    }

    fn resource_plan(&self, config: &SandboxCreateConfig) -> Result<ResourcePlan, AgentError> {
        let cpus = match config.cpu_limit {
            None => self.default_vcpus,
            Some(limit) if !limit.is_finite() || limit <= 0.0 => {
                return Err(AgentError::Validation {
                    message: format!(
                        "cpu_limit {} for run {} must be a positive number of cores",
                        limit, config.run_id
                    ),
                })
            }
            Some(limit) if limit.ceil() > f64::from(u8::MAX) => {
                return Err(AgentError::Validation {
                    message: format!(
                        "cpu_limit {} for run {} exceeds the {} backend maximum of {} vCPUs",
                        limit,
                        config.run_id,
                        PROVIDER_NAME,
                        u8::MAX
                    ),
                })
            }
            Some(limit) => limit.ceil() as u8,
        };
        let memory_mib = match config.memory_limit_mb {
            None => self.default_memory_mib,
            Some(mib) if mib < MIN_MEMORY_MIB => {
                return Err(AgentError::Validation {
                    message: format!(
                        "memory_limit_mb {} for run {} is below the {} backend minimum of {} MiB",
                        mib, config.run_id, PROVIDER_NAME, MIN_MEMORY_MIB
                    ),
                })
            }
            Some(mib) => u32::try_from(mib).map_err(|_| AgentError::Validation {
                message: format!(
                    "memory_limit_mb {} for run {} exceeds the {} backend maximum",
                    mib, config.run_id, PROVIDER_NAME
                ),
            })?,
        };
        let disk_mib = config
            .disk_size_mb
            .map(|mib| {
                u32::try_from(mib).map_err(|_| AgentError::Validation {
                    message: format!(
                        "disk_size_mb {} for run {} exceeds the {} backend maximum",
                        mib, config.run_id, PROVIDER_NAME
                    ),
                })
            })
            .transpose()?;
        let nproc = match config.pids_limit {
            None => None,
            Some(limit) if limit <= 0 => {
                return Err(AgentError::Validation {
                    message: format!(
                        "pids_limit {} for run {} must be positive",
                        limit, config.run_id
                    ),
                })
            }
            Some(limit) => Some(limit as u64),
        };
        Ok(ResourcePlan {
            cpus,
            memory_mib,
            disk_mib,
            nproc,
        })
    }
}

/// Which callback, if any, receives exec output as it arrives.
enum OutputSink {
    None,
    /// Stdout lines only — the [`SandboxProvider::exec`] contract.
    Stdout(OnEventCallback),
    /// Both streams, tagged.
    Tagged(OnStreamEventCallback),
}

impl OutputSink {
    async fn emit(&self, stream: ExecStream, line: String) {
        match (self, stream) {
            (Self::None, _) | (Self::Stdout(_), ExecStream::Stderr) => {}
            (Self::Stdout(cb), ExecStream::Stdout) => cb(line).await,
            (Self::Tagged(cb), stream) => cb(stream, line).await,
        }
    }
}

/// Splits a byte stream into lines for callbacks and keeps a bounded copy
/// of everything seen for the final [`SandboxExecResult`].
#[derive(Default)]
struct StreamCapture {
    pending: Vec<u8>,
    captured: Vec<u8>,
    dropped: usize,
    /// The current line already exceeded [`MAX_LINE_BYTES`]; its remaining
    /// bytes are discarded up to the next newline.
    discarding: bool,
    /// Lines withheld from callbacks for exceeding [`MAX_LINE_BYTES`].
    oversized_lines: usize,
}

impl StreamCapture {
    /// Record a chunk; return the complete lines it finished.
    fn push(&mut self, chunk: &[u8]) -> Vec<String> {
        let room = MAX_CAPTURED_OUTPUT_BYTES.saturating_sub(self.captured.len());
        let kept = chunk.len().min(room);
        self.captured.extend_from_slice(&chunk[..kept]);
        self.dropped += chunk.len() - kept;

        let mut lines = Vec::new();
        let mut rest = chunk;
        while !rest.is_empty() {
            let newline = rest.iter().position(|b| *b == b'\n');
            let (segment, ends_line) = match newline {
                Some(pos) => (&rest[..pos], true),
                None => (rest, false),
            };
            rest = match newline {
                Some(pos) => &rest[pos + 1..],
                None => &[],
            };
            if !self.discarding {
                if self.pending.len() + segment.len() > MAX_LINE_BYTES {
                    // Never deliver a fragment: drop the whole record.
                    self.pending = Vec::new();
                    self.discarding = true;
                    self.oversized_lines += 1;
                } else {
                    self.pending.extend_from_slice(segment);
                }
            }
            if ends_line {
                if self.discarding {
                    self.discarding = false;
                } else {
                    lines.push(self.take_line());
                }
            }
        }
        lines
    }

    fn take_line(&mut self) -> String {
        let line = std::mem::take(&mut self.pending);
        String::from_utf8_lossy(&line)
            .trim_end_matches('\r')
            .to_string()
    }

    /// The trailing partial line, if the command didn't end with a newline.
    fn finish_line(&mut self) -> Option<String> {
        self.discarding = false;
        if self.pending.is_empty() {
            return None;
        }
        let rest = std::mem::take(&mut self.pending);
        Some(String::from_utf8_lossy(&rest).into_owned())
    }

    fn into_text(self) -> String {
        let mut text = String::from_utf8_lossy(&self.captured).into_owned();
        if self.dropped > 0 {
            text.push_str(&format!(
                "\n[temps: output truncated, {} further bytes not retained]\n",
                self.dropped
            ));
        }
        if self.oversized_lines > 0 {
            text.push_str(&format!(
                "\n[temps: {} output line(s) longer than {} bytes were not delivered to the stream]\n",
                self.oversized_lines, MAX_LINE_BYTES
            ));
        }
        text
    }
}

pub struct MicrosandboxSandboxProvider {
    config: MicrosandboxSandboxConfig,
    /// The one backend every SDK call is bound to.
    backend: Arc<dyn ::microsandbox::Backend>,
    /// Live agent connections by VM name. Connecting performs a handshake
    /// with the in-guest agent, so it's reused across calls. Entries are
    /// evicted on stop/destroy and on any SDK error, so a stale connection
    /// is re-established on the next call.
    live: Mutex<HashMap<String, Sandbox>>,
}

impl MicrosandboxSandboxProvider {
    /// Build the provider. Resolves configuration only — no files are
    /// created, no runtime is installed, nothing touches the network.
    pub fn new(config: MicrosandboxSandboxConfig) -> Result<Self, AgentError> {
        let backend = build_local_backend(&config)?;
        Ok(Self {
            config,
            backend: Arc::new(backend),
            live: Mutex::new(HashMap::new()),
        })
    }

    fn local(&self) -> Result<&LocalBackend, AgentError> {
        self.backend
            .as_local()
            .ok_or_else(|| AgentError::SandboxProviderUnavailable {
                provider: PROVIDER_NAME.to_string(),
                reason: "the microsandbox SDK backend is not a local backend".to_string(),
            })
    }

    /// Full availability probe: platform, hypervisor and the runtime pair
    /// under this provider's home.
    pub fn probe(&self) -> Result<(), MicrosandboxUnavailable> {
        host_virtualization()?;
        let home = self.config.home();
        let local = self
            .local()
            .map_err(|e| MicrosandboxUnavailable::InvalidConfiguration {
                home: home.display().to_string(),
                detail: e.to_string(),
            })?;
        resolve_runtime(local, &home).map(|_| ())
    }

    /// Install the pinned runtime pair (`msb` + `libkrunfw`) into this
    /// provider's home. Downloads the official release archive for this
    /// platform and verifies the installed pair. Idempotent: an existing
    /// complete installation is kept.
    pub async fn install_runtime(&self) -> Result<ResolvedRuntime, AgentError> {
        let home = self.config.home();
        std::fs::create_dir_all(&home)?;
        set_dir_private(&home);
        let local = self.local()?;
        let options = InstallOptions {
            version: MICROSANDBOX_VERSION.to_string(),
            ..InstallOptions::default()
        };
        ::microsandbox::setup::ensure_runtime(local.config(), options)
            .await
            .map_err(|e| AgentError::SandboxProviderUnavailable {
                provider: PROVIDER_NAME.to_string(),
                reason: format!(
                    "installing microsandbox runtime v{} into {} failed: {}",
                    MICROSANDBOX_VERSION,
                    home.display(),
                    e
                ),
            })
    }

    fn resolve_name(&self, config: &SandboxCreateConfig) -> String {
        match &config.container_name_override {
            Some(id) => format!("{}{}", MSB_SANDBOX_NAME_PREFIX, id),
            None => format!("{}{}", MSB_SANDBOX_NAME_PREFIX, config.run_id),
        }
    }

    fn handle_for(name: &str, image: String) -> SandboxHandle {
        SandboxHandle {
            node_id: None,
            sandbox_id: name.to_string(),
            sandbox_name: name.to_string(),
            work_dir: PathBuf::from(WORK_DIR),
            backend: SandboxBackend::Microsandbox,
            image,
        }
    }

    fn err(sandbox_name: &str, operation: &str, error: impl std::fmt::Display) -> AgentError {
        AgentError::SandboxExecFailed {
            run_id: 0,
            sandbox_id: sandbox_name.to_string(),
            reason: format!("{} {}: {}", PROVIDER_NAME, operation, error),
        }
    }

    fn cache_get(&self, name: &str) -> Option<Sandbox> {
        self.live
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(name)
            .cloned()
    }

    fn cache_put(&self, name: &str, sandbox: Sandbox) {
        self.live
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(name.to_string(), sandbox);
    }

    fn evict(&self, name: &str) {
        self.live
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(name);
    }

    /// Look the VM up in the SDK registry. `Ok(None)` when it doesn't exist.
    async fn lookup(&self, name: &str) -> Result<Option<MsbSandboxHandle>, AgentError> {
        match self
            .backend
            .sandboxes()
            .get(self.backend.clone(), name)
            .await
        {
            Ok(handle) => Ok(Some(handle)),
            Err(MicrosandboxError::SandboxNotFound(_)) => Ok(None),
            Err(e) => Err(Self::err(name, "lookup", e)),
        }
    }

    /// A live agent connection to a running VM.
    async fn connected(&self, name: &str) -> Result<Sandbox, AgentError> {
        if let Some(sandbox) = self.cache_get(name) {
            return Ok(sandbox);
        }
        let handle = self
            .lookup(name)
            .await?
            .ok_or(AgentError::SandboxNotFound {
                run_id: 0,
                sandbox: name.to_string(),
            })?;
        let sandbox = handle
            .connect_with_timeout(CONNECT_TIMEOUT)
            .await
            .map_err(|e| Self::err(name, "connect", e))?;
        self.cache_put(name, sandbox.clone());
        Ok(sandbox)
    }

    async fn run_exec(
        &self,
        handle: &SandboxHandle,
        cmd: Vec<String>,
        env: HashMap<String, String>,
        user: Option<&str>,
        sink: OutputSink,
    ) -> Result<SandboxExecResult, AgentError> {
        let name = handle.sandbox_name.as_str();
        let Some((program, args)) = cmd.split_first() else {
            return Err(AgentError::Validation {
                message: format!("exec in sandbox {} requires a command", name),
            });
        };
        validate_env_keys(name, env.keys())?;
        let sandbox = self.connected(name).await?;
        let cwd = handle.work_dir.to_string_lossy().into_owned();
        let user = user.map(str::to_string);
        let started = sandbox
            .exec_stream_with(program.clone(), |opts| {
                let opts = opts
                    .args(args.iter().cloned())
                    .cwd(cwd)
                    .envs(env)
                    .stdin_null();
                match user {
                    Some(user) => opts.user(user),
                    None => opts,
                }
            })
            .await;
        let mut exec = match started {
            Ok(exec) => exec,
            Err(e) => {
                self.evict(name);
                return Err(Self::err(name, &format!("exec '{}'", program), e));
            }
        };

        let mut stdout = StreamCapture::default();
        let mut stderr = StreamCapture::default();
        let exit_code = loop {
            match exec.recv().await {
                Some(ExecEvent::Stdout(chunk)) => {
                    for line in stdout.push(&chunk) {
                        sink.emit(ExecStream::Stdout, line).await;
                    }
                }
                Some(ExecEvent::Stderr(chunk)) => {
                    for line in stderr.push(&chunk) {
                        sink.emit(ExecStream::Stderr, line).await;
                    }
                }
                Some(ExecEvent::Exited { code }) => break code,
                Some(ExecEvent::Failed(failure)) => {
                    return Err(Self::err(
                        name,
                        &format!("exec '{}'", program),
                        format!("process failed to start: {:?}", failure),
                    ));
                }
                Some(ExecEvent::Started { .. }) | Some(ExecEvent::StdinError(_)) => {}
                None => {
                    self.evict(name);
                    return Err(Self::err(
                        name,
                        &format!("exec '{}'", program),
                        "output stream closed before the process reported an exit status",
                    ));
                }
            }
        };
        if let Some(line) = stdout.finish_line() {
            sink.emit(ExecStream::Stdout, line).await;
        }
        if let Some(line) = stderr.finish_line() {
            sink.emit(ExecStream::Stderr, line).await;
        }
        if stdout.oversized_lines + stderr.oversized_lines > 0 {
            tracing::warn!(
                sandbox = %name,
                program = %program,
                stdout_lines = stdout.oversized_lines,
                stderr_lines = stderr.oversized_lines,
                max_line_bytes = MAX_LINE_BYTES,
                "microsandbox exec output lines over the line cap were not delivered"
            );
        }
        Ok(SandboxExecResult {
            exit_code,
            stdout: stdout.into_text(),
            stderr: stderr.into_text(),
        })
    }

    async fn create_inner(
        &self,
        config: SandboxCreateConfig,
        name: &str,
        image: &str,
    ) -> Result<Sandbox, AgentError> {
        let plan = self.config.resource_plan(&config)?;
        let network = NetworkPlan::from_mode(config.network_mode.as_deref())?;
        validate_env_keys(name, config.env_vars.keys())?;
        let run_id = config.run_id;

        let mut builder = Sandbox::builder(name)
            .image(image)
            .cpus(plan.cpus)
            .memory(plan.memory_mib)
            .security(SecurityProfile::Restricted)
            .labels([
                ("sh.temps.backend", PROVIDER_NAME.to_string()),
                ("sh.temps.run_id", run_id.to_string()),
            ])
            .envs(config.env_vars)
            .patch(|p| p.mkdir(WORK_DIR, Some(0o755)))
            .replace();
        if let Some(disk) = plan.disk_mib {
            builder = builder.root_disk(disk);
        }
        if let Some(nproc) = plan.nproc {
            builder = builder.rlimit(RlimitResource::Nproc, nproc);
        }
        builder = match network.policy() {
            None => builder.disable_network(),
            Some(policy) => builder.network(|n| n.policy(policy)),
        };

        // Detached: the VM outlives this process, like Firecracker VMs, and
        // is recovered by name after a restart.
        ::microsandbox::with_backend(self.backend.clone(), builder.create_detached())
            .await
            .map_err(|e| AgentError::SandboxCreationFailed {
                run_id,
                provider: PROVIDER_NAME.to_string(),
                reason: format!("creating {} from image {}: {}", name, image, e),
            })
    }

    /// Stream one guest file to `host_path` chunk by chunk, so host memory
    /// stays at one chunk however large the file is.
    async fn download_file(
        &self,
        name: &str,
        guest_path: &str,
        host_path: &Path,
        mode: u32,
    ) -> Result<(), AgentError> {
        use std::os::unix::fs::PermissionsExt;
        use tokio::io::AsyncWriteExt;
        let operation = format!("download '{}' to '{}'", guest_path, host_path.display());
        let sandbox = self.connected(name).await?;
        let mut stream = sandbox.fs().read_stream(guest_path).await.map_err(|e| {
            self.evict(name);
            Self::err(name, &operation, e)
        })?;
        let mut file = tokio::fs::File::create(host_path)
            .await
            .map_err(|e| Self::err(name, &operation, e))?;
        while let Some(chunk) = stream.recv().await.map_err(|e| {
            self.evict(name);
            Self::err(name, &operation, e)
        })? {
            file.write_all(&chunk)
                .await
                .map_err(|e| Self::err(name, &operation, e))?;
        }
        file.flush()
            .await
            .map_err(|e| Self::err(name, &operation, e))?;
        tokio::fs::set_permissions(host_path, std::fs::Permissions::from_mode(mode))
            .await
            .map_err(|e| Self::err(name, &operation, e))
    }

    /// Mirror the guest work dir into `host_dir`; returns (files copied,
    /// entries removed). Guest symlinks and special files are never
    /// materialized on the host, and a host symlink is never written
    /// through, replaced or removed — seeding skipped them, so they are not
    /// the agent's.
    ///
    /// `.git` entries are never copied, replaced or removed, at any depth.
    /// The host runs `git` in this checkout afterwards; a guest-written Git
    /// config could name a command for it to run (`core.fsmonitor`, hooks,
    /// filter drivers). The agent's work still arrives: its edits and any
    /// commits it made land in the working tree, which host `git diff` /
    /// `ls-files` read against the untouched host repository.
    async fn mirror_workspace(
        &self,
        name: &str,
        host_dir: &Path,
    ) -> Result<(usize, usize), AgentError> {
        let sandbox = self.connected(name).await?;
        let fs = sandbox.fs();
        let (mut copied, mut removed) = (0usize, 0usize);
        // The host repository's identity, so a guest name the host
        // filesystem folds onto it (`.GIT` on a case-insensitive volume) is
        // recognised whatever spelling reached it.
        let host_git = file_identity(&host_dir.join(GIT_DIR_NAME));
        let mut stack = vec![(WORK_DIR.to_string(), host_dir.to_path_buf())];
        while let Some((guest_dir, local_dir)) = stack.pop() {
            let entries = fs.list(&guest_dir).await.map_err(|e| {
                self.evict(name);
                Self::err(name, &format!("list '{}'", guest_dir), e)
            })?;
            let mut present = std::collections::HashSet::new();
            for entry in entries {
                let Some(file_name) = guest_entry_name(&entry.path) else {
                    tracing::warn!(
                        sandbox = %name,
                        entry = %entry.path,
                        "skipping guest entry with an unsafe name during workspace sync"
                    );
                    continue;
                };
                present.insert(std::ffi::OsString::from(file_name));
                if is_git_metadata_name(file_name) {
                    continue;
                }
                let guest_path = format!("{}/{}", guest_dir, file_name);
                let local_path = local_dir.join(file_name);
                let local_meta = std::fs::symlink_metadata(&local_path).ok();
                if local_meta
                    .as_ref()
                    .is_some_and(|m| m.file_type().is_symlink())
                {
                    continue;
                }
                if host_git.is_some() && file_identity(&local_path) == host_git {
                    continue;
                }
                match entry.kind {
                    FsEntryKind::Directory => {
                        if local_meta.as_ref().is_some_and(|m| !m.is_dir()) {
                            std::fs::remove_file(&local_path)?;
                        }
                        std::fs::create_dir_all(&local_path)?;
                        stack.push((guest_path, local_path));
                    }
                    FsEntryKind::File => {
                        if local_meta.as_ref().is_some_and(|m| m.is_dir()) {
                            std::fs::remove_dir_all(&local_path)?;
                        }
                        self.download_file(name, &guest_path, &local_path, entry.mode & 0o777)
                            .await?;
                        copied += 1;
                    }
                    FsEntryKind::Symlink | FsEntryKind::Other => {}
                }
            }
            // What the guest no longer has, the agent deleted.
            for local in std::fs::read_dir(&local_dir)? {
                let local = local?;
                if present.contains(&local.file_name())
                    || is_git_metadata_name(&local.file_name().to_string_lossy())
                {
                    continue;
                }
                let meta = local.path().symlink_metadata()?;
                if meta.file_type().is_symlink() {
                    continue;
                }
                if meta.is_dir() {
                    std::fs::remove_dir_all(local.path())?;
                } else {
                    std::fs::remove_file(local.path())?;
                }
                removed += 1;
            }
        }
        Ok((copied, removed))
    }

    /// Stream one host file into the guest in fixed-size chunks, so host
    /// memory stays at one chunk however large the file is.
    async fn upload_file(
        &self,
        name: &str,
        host_path: &Path,
        guest_path: &str,
        mode: u32,
    ) -> Result<(), AgentError> {
        use tokio::io::AsyncReadExt;
        let operation = format!("upload '{}' to '{}'", host_path.display(), guest_path);
        let mut file = tokio::fs::File::open(host_path)
            .await
            .map_err(|e| Self::err(name, &operation, e))?;
        let sandbox = self.connected(name).await?;
        let fs = sandbox.fs();
        let result: Result<(), AgentError> = async {
            if let Some(parent) = Path::new(guest_path)
                .parent()
                .filter(|p| *p != Path::new("/"))
            {
                fs.mkdir(&parent.to_string_lossy())
                    .await
                    .map_err(|e| Self::err(name, &operation, e))?;
            }
            let sink = fs
                .write_stream(guest_path)
                .await
                .map_err(|e| Self::err(name, &operation, e))?;
            let mut buf = vec![0u8; UPLOAD_CHUNK_BYTES];
            loop {
                let read = file
                    .read(&mut buf)
                    .await
                    .map_err(|e| Self::err(name, &operation, e))?;
                if read == 0 {
                    break;
                }
                sink.write(&buf[..read])
                    .await
                    .map_err(|e| Self::err(name, &operation, e))?;
            }
            sink.close()
                .await
                .map_err(|e| Self::err(name, &operation, e))?;
            fs.set_stat(
                guest_path,
                false,
                FsSetAttrs {
                    mode: Some(mode),
                    ..FsSetAttrs::default()
                },
            )
            .await
            .map_err(|e| Self::err(name, &operation, e))
        }
        .await;
        if result.is_err() {
            self.evict(name);
        }
        result
    }

    /// Copy `host_work_dir` into the guest work dir. A missing or empty
    /// directory is a valid "start with an empty workspace" request.
    async fn seed_workspace(
        &self,
        handle: &SandboxHandle,
        run_id: i32,
        host_work_dir: &Path,
    ) -> Result<(), AgentError> {
        if !workspace_has_entries(host_work_dir) {
            return Ok(());
        }
        let started = Instant::now();
        self.write_directory(handle, host_work_dir, WORK_DIR)
            .await
            .map_err(|e| AgentError::SandboxCreationFailed {
                run_id,
                provider: PROVIDER_NAME.to_string(),
                reason: format!(
                    "copying workspace {} into {}:{}: {}",
                    host_work_dir.display(),
                    handle.sandbox_name,
                    WORK_DIR,
                    e
                ),
            })?;
        tracing::info!(
            sandbox = %handle.sandbox_name,
            source = %host_work_dir.display(),
            elapsed_ms = started.elapsed().as_millis() as u64,
            "microsandbox workspace seeded"
        );
        Ok(())
    }

    fn ensure_home(&self) -> Result<(), AgentError> {
        let home = self.config.home();
        std::fs::create_dir_all(&home)?;
        set_dir_private(&home);
        Ok(())
    }
}

#[async_trait]
impl SandboxProvider for MicrosandboxSandboxProvider {
    async fn create(&self, config: SandboxCreateConfig) -> Result<SandboxHandle, AgentError> {
        let run_id = config.run_id;
        // An explicit create on an unready host fails with the precise
        // reason — never a silent fallback to another backend.
        self.probe()
            .map_err(|e| AgentError::SandboxCreationFailed {
                run_id,
                provider: PROVIDER_NAME.to_string(),
                reason: e.to_string(),
            })?;
        self.ensure_home()?;

        let name = self.resolve_name(&config);
        let image = config
            .image
            .clone()
            .filter(|i| !i.is_empty())
            .unwrap_or_else(|| DEFAULT_IMAGE.to_string());
        self.evict(&name);

        let started = Instant::now();
        let host_work_dir = config.host_work_dir.clone();
        let sandbox = self.create_inner(config, &name, &image).await?;
        self.cache_put(&name, sandbox);
        let handle = Self::handle_for(&name, image.clone());

        // Docker bind-mounts `host_work_dir` at the work dir; a VM can't, so
        // copy the prepared tree (e.g. the cloned repository) in before the
        // caller sees the handle. A VM without its workspace is useless, so
        // a failed copy tears it down rather than returning it.
        if let Err(error) = self.seed_workspace(&handle, run_id, &host_work_dir).await {
            if let Err(cleanup) = self.destroy(&handle, true).await {
                tracing::warn!(
                    sandbox = %name,
                    "failed to destroy microsandbox sandbox after workspace seeding failed: {}",
                    cleanup
                );
            }
            return Err(error);
        }
        tracing::info!(
            sandbox = %name,
            image = %image,
            elapsed_ms = started.elapsed().as_millis() as u64,
            "microsandbox sandbox up"
        );
        Ok(handle)
    }

    async fn exec(
        &self,
        handle: &SandboxHandle,
        cmd: Vec<String>,
        env: HashMap<String, String>,
        on_output: Option<OnEventCallback>,
    ) -> Result<SandboxExecResult, AgentError> {
        let sink = on_output.map_or(OutputSink::None, OutputSink::Stdout);
        self.run_exec(handle, cmd, env, None, sink).await
    }

    async fn exec_as_root(
        &self,
        handle: &SandboxHandle,
        cmd: Vec<String>,
        env: HashMap<String, String>,
        on_output: Option<OnEventCallback>,
    ) -> Result<SandboxExecResult, AgentError> {
        let sink = on_output.map_or(OutputSink::None, OutputSink::Stdout);
        self.run_exec(handle, cmd, env, Some("root"), sink).await
    }

    async fn exec_as_user(
        &self,
        handle: &SandboxHandle,
        user: &str,
        cmd: Vec<String>,
        env: HashMap<String, String>,
        on_output: Option<OnEventCallback>,
    ) -> Result<SandboxExecResult, AgentError> {
        let sink = on_output.map_or(OutputSink::None, OutputSink::Stdout);
        self.run_exec(handle, cmd, env, Some(user), sink).await
    }

    async fn exec_streamed(
        &self,
        handle: &SandboxHandle,
        cmd: Vec<String>,
        env: HashMap<String, String>,
        on_event: Option<OnStreamEventCallback>,
    ) -> Result<SandboxExecResult, AgentError> {
        let sink = on_event.map_or(OutputSink::None, OutputSink::Tagged);
        self.run_exec(handle, cmd, env, None, sink).await
    }

    async fn is_alive(&self, handle: &SandboxHandle) -> Result<bool, AgentError> {
        Ok(self
            .lookup(&handle.sandbox_name)
            .await?
            .is_some_and(|h| h.status_snapshot() == SandboxStatus::Running))
    }

    async fn write_file(
        &self,
        handle: &SandboxHandle,
        path: &str,
        contents: &[u8],
        mode: u32,
    ) -> Result<(), AgentError> {
        let name = handle.sandbox_name.as_str();
        let sandbox = self.connected(name).await?;
        let fs = sandbox.fs();
        let result = async {
            if let Some(parent) = Path::new(path).parent().filter(|p| *p != Path::new("/")) {
                fs.mkdir(&parent.to_string_lossy()).await?;
            }
            fs.write(path, contents).await?;
            fs.set_stat(
                path,
                false,
                FsSetAttrs {
                    mode: Some(mode),
                    ..FsSetAttrs::default()
                },
            )
            .await
        }
        .await;
        result.map_err(|e| {
            self.evict(name);
            Self::err(name, &format!("write_file '{}'", path), e)
        })
    }

    async fn read_file(&self, handle: &SandboxHandle, path: &str) -> Result<Vec<u8>, AgentError> {
        let name = handle.sandbox_name.as_str();
        let sandbox = self.connected(name).await?;
        sandbox
            .fs()
            .read(path)
            .await
            .map(|b| b.to_vec())
            .map_err(|e| {
                self.evict(name);
                Self::err(name, &format!("read_file '{}'", path), e)
            })
    }

    async fn read_file_bounded(
        &self,
        handle: &SandboxHandle,
        path: &str,
        max_bytes: u64,
    ) -> Result<Vec<u8>, AgentError> {
        // Stat first to reject an oversized file without reading it, then
        // stream and stop at the limit: the file may grow between the two,
        // and host memory must never exceed `max_bytes` plus one chunk.
        let name = handle.sandbox_name.as_str();
        let sandbox = self.connected(name).await?;
        let fs = sandbox.fs();
        let size = fs
            .stat(path)
            .await
            .map_err(|e| Self::err(name, &format!("stat '{}'", path), e))?
            .size;
        if size > max_bytes {
            return Err(crate::sandbox::file_too_large(handle, path, max_bytes));
        }
        let operation = format!("read_file_bounded '{}'", path);
        let mut stream = fs.read_stream(path).await.map_err(|e| {
            self.evict(name);
            Self::err(name, &operation, e)
        })?;
        let mut contents = Vec::with_capacity(usize::try_from(size).unwrap_or(0));
        while let Some(chunk) = stream.recv().await.map_err(|e| {
            self.evict(name);
            Self::err(name, &operation, e)
        })? {
            if (contents.len() + chunk.len()) as u64 > max_bytes {
                return Err(crate::sandbox::file_too_large(handle, path, max_bytes));
            }
            contents.extend_from_slice(&chunk);
        }
        Ok(contents)
    }

    async fn write_directory(
        &self,
        handle: &SandboxHandle,
        local_dir: &Path,
        target_path: &str,
    ) -> Result<(), AgentError> {
        // Per-file streamed writes over the agent's fs channel. Symlinks are
        // skipped (not followed) so a link
        // in the source tree can't pull host files outside `local_dir` in.
        let mut stack = vec![local_dir.to_path_buf()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir)? {
                let entry = entry?;
                let path = entry.path();
                let rel = path.strip_prefix(local_dir).map_err(|e| {
                    Self::err(
                        &handle.sandbox_name,
                        &format!("write_directory '{}'", local_dir.display()),
                        e,
                    )
                })?;
                let target = format!("{}/{}", target_path.trim_end_matches('/'), rel.display());
                let meta = entry.path().symlink_metadata()?;
                if meta.is_dir() {
                    stack.push(path);
                } else if meta.is_file() {
                    use std::os::unix::fs::PermissionsExt;
                    self.upload_file(
                        &handle.sandbox_name,
                        &path,
                        &target,
                        meta.permissions().mode() & 0o777,
                    )
                    .await?;
                }
            }
        }
        Ok(())
    }

    async fn sync_workspace_to_host(
        &self,
        handle: &SandboxHandle,
        host_dir: &Path,
    ) -> Result<(), AgentError> {
        // The guest copy is authoritative after a run: Docker would have
        // written these edits straight into `host_dir` through its bind mount.
        let name = handle.sandbox_name.as_str();
        let started = Instant::now();
        let (copied, removed) = self.mirror_workspace(name, host_dir).await.map_err(|e| {
            AgentError::SandboxExecFailed {
                run_id: 0,
                sandbox_id: name.to_string(),
                reason: format!(
                    "{} syncing {} back to {}: {}",
                    PROVIDER_NAME,
                    WORK_DIR,
                    host_dir.display(),
                    e
                ),
            }
        })?;
        tracing::info!(
            sandbox = %name,
            host_dir = %host_dir.display(),
            copied,
            removed,
            elapsed_ms = started.elapsed().as_millis() as u64,
            "microsandbox workspace synced to host"
        );
        Ok(())
    }

    async fn kill_processes(
        &self,
        handle: &SandboxHandle,
        pattern: &str,
        signal: KillSignal,
    ) -> Result<(), AgentError> {
        // Best-effort by contract: pkill exits 1 when nothing matched.
        let _ = self
            .run_exec(
                handle,
                vec![
                    "pkill".to_string(),
                    format!("-{}", signal.as_number()),
                    "-f".to_string(),
                    pattern.to_string(),
                ],
                HashMap::new(),
                Some("root"),
                OutputSink::None,
            )
            .await?;
        Ok(())
    }

    async fn destroy(
        &self,
        handle: &SandboxHandle,
        _purge_volumes: bool,
    ) -> Result<(), AgentError> {
        // No named volumes are created for this backend: the VM's root disk
        // is its only storage and goes with it.
        let name = handle.sandbox_name.as_str();
        self.evict(name);
        let Some(vm) = self.lookup(name).await? else {
            return Ok(());
        };
        match vm.destroy().await {
            Ok(()) | Err(MicrosandboxError::SandboxNotFound(_)) => {
                tracing::info!(sandbox = %name, "microsandbox sandbox destroyed");
                Ok(())
            }
            Err(e) => Err(Self::err(name, "destroy", e)),
        }
    }

    async fn stop(&self, handle: &SandboxHandle) -> Result<(), AgentError> {
        let name = handle.sandbox_name.as_str();
        self.evict(name);
        let Some(vm) = self.lookup(name).await? else {
            return Err(AgentError::SandboxNotFound {
                run_id: 0,
                sandbox: name.to_string(),
            });
        };
        if !matches!(
            vm.status_snapshot(),
            SandboxStatus::Running | SandboxStatus::Starting | SandboxStatus::Draining
        ) {
            return Ok(());
        }
        vm.stop().await.map_err(|e| Self::err(name, "stop", e))
    }

    async fn start(&self, handle: &SandboxHandle) -> Result<(), AgentError> {
        let name = handle.sandbox_name.as_str();
        let Some(vm) = self.lookup(name).await? else {
            return Err(AgentError::SandboxNotFound {
                run_id: 0,
                sandbox: name.to_string(),
            });
        };
        self.evict(name);
        let sandbox = vm
            .connect_or_start_detached()
            .await
            .map_err(|e| Self::err(name, "start", e))?;
        self.cache_put(name, sandbox);
        Ok(())
    }

    async fn recover(&self, run_id: i32) -> Result<Option<SandboxHandle>, AgentError> {
        self.recover_by_name(&format!("{}{}", MSB_SANDBOX_NAME_PREFIX, run_id))
            .await
    }

    async fn recover_by_name(
        &self,
        container_name: &str,
    ) -> Result<Option<SandboxHandle>, AgentError> {
        // Accept both the full VM name and the bare label the standalone
        // registry passes.
        let name = if container_name.starts_with(MSB_SANDBOX_NAME_PREFIX) {
            container_name.to_string()
        } else {
            format!("{}{}", MSB_SANDBOX_NAME_PREFIX, container_name)
        };
        // An unready host has nothing to recover; don't let the SDK create
        // its state directory just to answer "no".
        if !self.config.home().exists() {
            return Ok(None);
        }
        Ok(self
            .lookup(&name)
            .await?
            .map(|_| Self::handle_for(&name, String::new())))
    }

    fn supports_backend(&self, backend: SandboxBackend) -> bool {
        backend == SandboxBackend::Microsandbox
    }

    fn name(&self) -> &str {
        PROVIDER_NAME
    }

    async fn is_available(&self) -> bool {
        self.probe().is_ok()
    }

    async fn image_status(&self) -> Result<(bool, String), AgentError> {
        // Images are pulled per sandbox by the SDK; there is no build step.
        Ok((self.is_available().await, DEFAULT_IMAGE.to_string()))
    }

    async fn rebuild_image(&self) -> Result<String, AgentError> {
        Ok(DEFAULT_IMAGE.to_string())
    }
}

/// Build the SDK backend bound to `<data_dir>/microsandbox`. Lazy: the SDK
/// database and directories are only created on first sandbox operation.
fn build_local_backend(config: &MicrosandboxSandboxConfig) -> Result<LocalBackend, AgentError> {
    let home = config.home();
    LocalBackend::builder()
        .home(&home)
        .config_path(config.config_path())
        // Host-side isolation floor: no host vsock routes, bounded
        // per-sandbox connection tables. Applied to every VM this backend
        // launches regardless of what a sandbox spec requests.
        .deployment_profile(DeploymentProfile::MultiTenant)
        .build_lazy()
        .map_err(|e| {
            AgentError::from(MicrosandboxUnavailable::InvalidConfiguration {
                home: home.display().to_string(),
                detail: e.to_string(),
            })
        })
}

fn resolve_runtime(
    local: &LocalBackend,
    home: &Path,
) -> Result<ResolvedRuntime, MicrosandboxUnavailable> {
    ::microsandbox::setup::resolve_runtime(local.config()).map_err(|e| match e {
        MicrosandboxError::RuntimeNotInstalled(detail) => {
            MicrosandboxUnavailable::RuntimeNotInstalled {
                home: home.display().to_string(),
                version: MICROSANDBOX_VERSION.to_string(),
                detail,
            }
        }
        other => MicrosandboxUnavailable::RuntimeNotInstalled {
            home: home.display().to_string(),
            version: MICROSANDBOX_VERSION.to_string(),
            detail: other.to_string(),
        },
    })
}

/// The SDK reserves the `MSB_` environment prefix for its own runtime
/// configuration and rejects it at build time; reject it up front with the
/// offending key named.
fn validate_env_keys<'a>(
    sandbox: &str,
    keys: impl IntoIterator<Item = &'a String>,
) -> Result<(), AgentError> {
    if let Some(key) = keys.into_iter().find(|k| k.starts_with("MSB_")) {
        return Err(AgentError::Validation {
            message: format!(
                "environment variable '{}' for sandbox {} uses the MSB_ prefix, \
                 which the {} backend reserves for its runtime",
                key, sandbox, PROVIDER_NAME
            ),
        });
    }
    Ok(())
}

/// Whether the pinned runtime pair is installed under `config`'s home.
pub(super) fn runtime_probe(
    config: &MicrosandboxSandboxConfig,
) -> Result<(), MicrosandboxUnavailable> {
    let backend =
        build_local_backend(config).map_err(|e| MicrosandboxUnavailable::InvalidConfiguration {
            home: config.home().display().to_string(),
            detail: e.to_string(),
        })?;
    resolve_runtime(&backend, &config.home()).map(|_| ())
}

/// Restrict a directory to the owner (0700). The SDK home holds the SDK
/// database, which records each sandbox's environment (including injected
/// credentials). Best-effort — a failure is logged, not fatal.
fn set_dir_private(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Err(e) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)) {
        tracing::warn!("failed to 0700 {}: {}", path.display(), e);
    }
}

/// Git repository metadata — never synced from a guest; see
/// [`MicrosandboxSandboxProvider::mirror_workspace`].
const GIT_DIR_NAME: &str = ".git";

/// Whether a host filesystem could resolve `name` to `.git`. The guest is
/// case-sensitive Linux; the host may not be. Mirrors Git's own guards
/// (`is_hfs_dotgit` / `is_ntfs_dotgit`): ASCII case folding, code points
/// HFS+ ignores, trailing dots and spaces, and the 8.3 short name.
fn is_git_metadata_name(name: &str) -> bool {
    let visible: String = name.chars().filter(|c| !is_hfs_ignorable(*c)).collect();
    let folded = visible.trim_end_matches(['.', ' ']).to_ascii_lowercase();
    folded == GIT_DIR_NAME || folded == "git~1"
}

/// Code points HFS+ drops when comparing names.
fn is_hfs_ignorable(c: char) -> bool {
    matches!(
        c,
        '\u{200C}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{206A}'..='\u{206F}' | '\u{FEFF}'
    )
}

/// (device, inode) of `path` without following a final symlink.
fn file_identity(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    std::fs::symlink_metadata(path)
        .ok()
        .map(|m| (m.dev(), m.ino()))
}

/// The final component of a guest directory-listing path, if it is a
/// plain name. Guest-reported names are untrusted: anything that could
/// address outside the directory being mirrored is refused.
fn guest_entry_name(path: &str) -> Option<&str> {
    let name = path.rsplit('/').next()?;
    (!name.is_empty() && name != "." && name != ".." && !name.contains('\0')).then_some(name)
}

/// Whether `dir` is an existing directory with at least one entry — i.e.
/// there is a prepared workspace to copy into a new VM.
fn workspace_has_entries(dir: &Path) -> bool {
    std::fs::read_dir(dir).is_ok_and(|mut entries| entries.next().is_some())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::microsandbox::host_data_dir;

    fn create_config() -> SandboxCreateConfig {
        SandboxCreateConfig {
            node_id: None,
            owner_user_id: None,
            run_id: 7,
            container_name_override: None,
            // Nonexistent: create seeds nothing unless a test opts in.
            host_work_dir: PathBuf::from("/nonexistent/temps-msb-test-workspace"),
            workspace_volume: None,
            image: None,
            cpu_limit: None,
            memory_limit_mb: None,
            pids_limit: None,
            disk_size_mb: None,
            network_mode: None,
            env_vars: HashMap::new(),
            idle_timeout: Duration::from_secs(60),
            backend: Some(SandboxBackend::Microsandbox),
        }
    }

    fn provider_at(data_dir: PathBuf) -> MicrosandboxSandboxProvider {
        MicrosandboxSandboxProvider::new(MicrosandboxSandboxConfig::from_data_dir(data_dir))
            .expect("provider construction resolves config only")
    }

    #[test]
    fn resolve_name_prefers_override() {
        let p = provider_at(PathBuf::from("/nonexistent"));
        let mut config = create_config();
        config.container_name_override = Some("abc123".to_string());
        assert_eq!(p.resolve_name(&config), "temps-msbsandbox-abc123");
        config.container_name_override = None;
        assert_eq!(p.resolve_name(&config), "temps-msbsandbox-7");
    }

    #[test]
    fn prefix_is_distinct_from_other_backends() {
        assert!(!MSB_SANDBOX_NAME_PREFIX
            .starts_with(crate::sandbox::firecracker::FC_SANDBOX_NAME_PREFIX));
        assert!(!MSB_SANDBOX_NAME_PREFIX.starts_with("temps-sandbox-"));
        // Every resolved name must be a valid SDK sandbox name.
        let name = format!("{}{}", MSB_SANDBOX_NAME_PREFIX, "sbx_0123456789abcdef");
        assert!(::microsandbox::validate_sandbox_name(&name).is_ok());
    }

    #[test]
    fn handles_are_stamped_with_the_microsandbox_backend() {
        let handle = MicrosandboxSandboxProvider::handle_for("temps-msbsandbox-1", "alpine".into());
        assert_eq!(handle.backend, SandboxBackend::Microsandbox);
        assert_eq!(handle.work_dir, PathBuf::from("/workspace"));
        assert_eq!(handle.node_id, None);
    }

    #[test]
    fn supports_only_its_own_backend() {
        let p = provider_at(PathBuf::from("/nonexistent"));
        assert!(p.supports_backend(SandboxBackend::Microsandbox));
        assert!(!p.supports_backend(SandboxBackend::Docker));
        assert!(!p.supports_backend(SandboxBackend::Firecracker));
        assert!(!p.supports_backend(SandboxBackend::Local));
    }

    #[tokio::test]
    async fn recover_by_name_accepts_bare_label_without_creating_state() {
        let dir = tempfile::tempdir().unwrap();
        let p = provider_at(dir.path().to_path_buf());
        assert!(p.recover_by_name("abc").await.unwrap().is_none());
        assert!(p
            .recover_by_name("temps-msbsandbox-abc")
            .await
            .unwrap()
            .is_none());
        assert!(
            !dir.path().join("microsandbox").exists(),
            "recovery on an unprovisioned host must not create SDK state"
        );
    }

    #[test]
    fn network_modes_map_to_host_side_policies() {
        assert_eq!(
            NetworkPlan::from_mode(None).unwrap(),
            NetworkPlan::PublicOnly
        );
        assert_eq!(
            NetworkPlan::from_mode(Some("full")).unwrap(),
            NetworkPlan::PublicOnly
        );
        assert_eq!(
            NetworkPlan::from_mode(Some("open")).unwrap(),
            NetworkPlan::PublicOnly
        );
        assert_eq!(
            NetworkPlan::from_mode(Some("restricted")).unwrap(),
            NetworkPlan::HostOnly
        );
        assert_eq!(
            NetworkPlan::from_mode(Some("none")).unwrap(),
            NetworkPlan::Disabled
        );
        assert!(NetworkPlan::Disabled.policy().is_none());
    }

    #[test]
    fn unknown_network_mode_is_rejected_with_the_value() {
        match NetworkPlan::from_mode(Some("bridge0")) {
            Err(AgentError::Validation { message }) => {
                assert!(message.contains("bridge0"), "{message}");
                assert!(message.contains("microsandbox"), "{message}");
            }
            other => panic!("expected validation error, got {other:?}"),
        }
    }

    /// Unrestricted egress groups (any port, any protocol) a policy allows,
    /// plus its default egress action. Policy evaluation lives behind the
    /// SDK's `engine` feature, which this crate deliberately doesn't build,
    /// so the policy is checked structurally.
    fn egress_shape(policy: &NetworkPolicy) -> (String, Vec<String>) {
        let value = serde_json::to_value(policy).unwrap();
        let groups = value["rules"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["direction"] == "egress" && r["action"] == "allow")
            .filter(|r| r["ports"].as_array().is_none_or(|p| p.is_empty()))
            .filter_map(|r| r["destination"]["group"].as_str().map(str::to_string))
            .collect();
        (
            value["default_egress"].as_str().unwrap().to_string(),
            groups,
        )
    }

    #[test]
    fn full_policy_allows_only_the_public_internet() {
        let (default, groups) = egress_shape(&NetworkPlan::PublicOnly.policy().unwrap());
        assert_eq!(default, "deny");
        // Host, private/LAN, loopback, link-local and metadata fall through
        // to the deny default.
        assert_eq!(groups, vec!["public".to_string()]);
    }

    #[test]
    fn restricted_policy_reaches_only_the_sandbox_host() {
        let (default, groups) = egress_shape(&NetworkPlan::HostOnly.policy().unwrap());
        assert_eq!(default, "deny");
        assert_eq!(groups, vec!["host".to_string()]);
    }

    #[test]
    fn resource_plan_defaults_and_rounding() {
        let cfg = MicrosandboxSandboxConfig::from_data_dir(PathBuf::from("/x"));
        let mut c = create_config();
        let plan = cfg.resource_plan(&c).unwrap();
        assert_eq!(plan.cpus, 1);
        assert_eq!(plan.memory_mib, 512);
        assert_eq!(plan.disk_mib, None);
        assert_eq!(plan.nproc, None);

        c.cpu_limit = Some(1.5);
        c.memory_limit_mb = Some(2048);
        c.disk_size_mb = Some(4096);
        c.pids_limit = Some(256);
        let plan = cfg.resource_plan(&c).unwrap();
        assert_eq!(plan.cpus, 2, "fractional cores round up to whole vCPUs");
        assert_eq!(plan.memory_mib, 2048);
        assert_eq!(plan.disk_mib, Some(4096));
        assert_eq!(plan.nproc, Some(256));
    }

    #[test]
    fn resource_plan_rejects_out_of_range_values_with_run_context() {
        let cfg = MicrosandboxSandboxConfig::from_data_dir(PathBuf::from("/x"));
        type Mutation = Box<dyn Fn(&mut SandboxCreateConfig)>;
        let cases: Vec<(Mutation, &str)> = vec![
            (Box::new(|c| c.cpu_limit = Some(0.0)), "cpu_limit"),
            (Box::new(|c| c.cpu_limit = Some(f64::NAN)), "cpu_limit"),
            (Box::new(|c| c.cpu_limit = Some(1000.0)), "cpu_limit"),
            (
                Box::new(|c| c.memory_limit_mb = Some(64)),
                "memory_limit_mb",
            ),
            (
                Box::new(|c| c.memory_limit_mb = Some(u64::from(u32::MAX) + 1)),
                "memory_limit_mb",
            ),
            (
                Box::new(|c| c.disk_size_mb = Some(u64::from(u32::MAX) + 1)),
                "disk_size_mb",
            ),
            (Box::new(|c| c.pids_limit = Some(0)), "pids_limit"),
        ];
        for (mutate, field) in cases {
            let mut c = create_config();
            mutate(&mut c);
            match cfg.resource_plan(&c) {
                Err(AgentError::Validation { message }) => {
                    assert!(message.contains(field), "{message}");
                    assert!(message.contains("run 7"), "{message}");
                }
                other => panic!("expected validation error for {field}, got {other:?}"),
            }
        }
    }

    #[test]
    fn reserved_env_prefix_is_rejected_by_name() {
        let keys = ["ANTHROPIC_API_KEY".to_string(), "MSB_PATH".to_string()];
        match validate_env_keys("temps-msbsandbox-7", keys.iter()) {
            Err(AgentError::Validation { message }) => {
                assert!(message.contains("MSB_PATH"), "{message}");
                assert!(message.contains("temps-msbsandbox-7"), "{message}");
            }
            other => panic!("expected validation error, got {other:?}"),
        }
        assert!(validate_env_keys("s", ["HOME".to_string()].iter()).is_ok());
    }

    #[test]
    fn stream_capture_splits_lines_across_chunks() {
        let mut s = StreamCapture::default();
        assert!(s.push(b"hel").is_empty());
        assert_eq!(s.push(b"lo\nwor"), vec!["hello".to_string()]);
        assert_eq!(s.push(b"ld\r\nx"), vec!["world".to_string()]);
        assert_eq!(s.finish_line().as_deref(), Some("x"));
        assert_eq!(s.finish_line(), None);
        assert_eq!(s.into_text(), "hello\nworld\r\nx");
    }

    #[test]
    fn stream_capture_bounds_retained_output() {
        let mut s = StreamCapture::default();
        let chunk = vec![b'a'; MAX_CAPTURED_OUTPUT_BYTES + 10];
        let _ = s.push(&chunk);
        let text = s.into_text();
        assert!(
            text.contains("10 further bytes not retained"),
            "tail: {}",
            &text[text.len() - 80..]
        );
        assert!(text.len() < MAX_CAPTURED_OUTPUT_BYTES + 200);
    }

    #[test]
    fn stream_capture_bounds_newline_free_output() {
        // Far more newline-free output than the line cap, in many chunks:
        // the partial-line buffer stays bounded and nothing is delivered
        // as a fragment.
        let mut s = StreamCapture::default();
        let chunk = vec![b'z'; 1024 * 1024];
        for _ in 0..(MAX_LINE_BYTES / chunk.len() + 4) {
            assert!(s.push(&chunk).is_empty(), "no fragments");
            assert!(s.pending.len() <= MAX_LINE_BYTES);
        }
        assert_eq!(s.finish_line(), None);
        assert_eq!(s.oversized_lines, 1);
        assert!(s
            .into_text()
            .contains("1 output line(s) longer than 8388608 bytes were not delivered"));
    }

    #[test]
    fn stream_capture_drops_an_oversized_record_whole_and_resumes() {
        let mut s = StreamCapture::default();
        let big = format!("{{\"text\":\"{}\"}}", "x".repeat(MAX_LINE_BYTES));
        let mut lines = s.push(b"{\"type\":\"start\"}\n");
        // The oversized record arrives in pieces, then normal records follow.
        for piece in big.as_bytes().chunks(300_000) {
            lines.extend(s.push(piece));
        }
        lines.extend(s.push(b"\n{\"type\":\"end\"}\n"));
        assert_eq!(lines, vec!["{\"type\":\"start\"}", "{\"type\":\"end\"}"]);
        assert_eq!(s.oversized_lines, 1);
    }

    #[test]
    fn stream_capture_keeps_a_record_at_the_line_cap_whole() {
        let mut s = StreamCapture::default();
        let mut chunk = vec![b'q'; MAX_LINE_BYTES];
        chunk.extend_from_slice(b"\nnext\n");
        let lines = s.push(&chunk);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].len(), MAX_LINE_BYTES);
        assert_eq!(lines[1], "next");
        assert_eq!(s.oversized_lines, 0);
    }

    // ── End-to-end: real microVMs ──────────────────────────────────────
    //
    // These boot real VMs through the provider. Like the Docker tests they
    // skip at runtime — printing why — when this host can't run the
    // backend (no hypervisor, or no runtime: run `temps microsandbox
    // setup`). They use the server's data dir so a runtime installed by
    // setup is found. Every VM is destroyed even when an assertion fails.

    /// A ready provider, or `None` after printing why the test is skipped.
    fn e2e_provider() -> Option<MicrosandboxSandboxProvider> {
        let config = MicrosandboxSandboxConfig::from_data_dir(host_data_dir());
        let provider = match MicrosandboxSandboxProvider::new(config) {
            Ok(provider) => provider,
            Err(e) => {
                println!("microsandbox not available, skipping: {e}");
                return None;
            }
        };
        if let Err(reason) = provider.probe() {
            println!("microsandbox not available, skipping: {reason}");
            return None;
        }
        Some(provider)
    }

    fn e2e_config(label: &str, network_mode: &str) -> SandboxCreateConfig {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or_default();
        let mut c = create_config();
        c.container_name_override = Some(format!(
            "e2e-{}-{}-{:08x}",
            label,
            std::process::id(),
            nanos
        ));
        c.image = Some("alpine:3.20".into());
        c.memory_limit_mb = Some(256);
        c.network_mode = Some(network_mode.into());
        c.env_vars
            .insert("TEMPS_E2E_MARKER".into(), "from-create".into());
        c
    }

    /// Run `body` against a fresh sandbox, then destroy it whatever happened.
    async fn with_sandbox<F, Fut>(
        provider: &MicrosandboxSandboxProvider,
        config: SandboxCreateConfig,
        body: F,
    ) where
        F: FnOnce(SandboxHandle) -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        use futures::FutureExt;
        let started = Instant::now();
        let handle = provider.create(config).await.expect("create sandbox");
        println!(
            "created {} in {} ms (includes image resolution)",
            handle.sandbox_name,
            started.elapsed().as_millis()
        );
        let outcome = std::panic::AssertUnwindSafe(body(handle.clone()))
            .catch_unwind()
            .await;
        provider
            .destroy(&handle, true)
            .await
            .expect("destroy sandbox");
        if let Err(panic) = outcome {
            std::panic::resume_unwind(panic);
        }
        assert!(!provider.is_alive(&handle).await.unwrap());
        assert!(provider
            .recover_by_name(&handle.sandbox_name)
            .await
            .unwrap()
            .is_none());
    }

    async fn sh(
        provider: &MicrosandboxSandboxProvider,
        handle: &SandboxHandle,
        script: &str,
    ) -> SandboxExecResult {
        provider
            .exec(
                handle,
                vec!["sh".into(), "-c".into(), script.into()],
                HashMap::new(),
                None,
            )
            .await
            .unwrap_or_else(|e| panic!("exec `{script}`: {e}"))
    }

    const EGRESS_PROBE: &str = "wget -q -T 5 -O - http://1.1.1.1/cdn-cgi/trace";
    /// Number of IPv4 routes in the guest (header line excluded).
    const ROUTE_COUNT: &str = "tail -n +2 /proc/net/route | wc -l";

    #[tokio::test]
    async fn e2e_lifecycle_exec_files_and_isolated_network() {
        let Some(provider) = e2e_provider() else {
            return;
        };
        with_sandbox(&provider, e2e_config("life", "none"), |handle| {
            let provider = &provider;
            async move {
                assert_eq!(handle.backend, SandboxBackend::Microsandbox);
                assert!(handle.sandbox_name.starts_with(MSB_SANDBOX_NAME_PREFIX));
                assert!(provider.is_alive(&handle).await.unwrap());

                // A Linux guest kernel, regardless of the host OS.
                let uname = provider
                    .exec(
                        &handle,
                        vec!["uname".into(), "-a".into()],
                        HashMap::new(),
                        None,
                    )
                    .await
                    .unwrap();
                println!("guest: {}", uname.stdout.trim());
                assert_eq!(uname.exit_code, 0);
                assert!(uname.stdout.starts_with("Linux "), "{}", uname.stdout);

                // Commands run in the work dir with create-time env, and
                // per-exec env is layered on top.
                let pwd = sh(provider, &handle, "pwd; echo $TEMPS_E2E_MARKER").await;
                assert_eq!(pwd.stdout, "/workspace\nfrom-create\n");
                let env = provider
                    .exec(
                        &handle,
                        vec!["sh".into(), "-c".into(), "echo $PER_EXEC".into()],
                        HashMap::from([("PER_EXEC".to_string(), "layered".to_string())]),
                        None,
                    )
                    .await
                    .unwrap();
                assert_eq!(env.stdout.trim(), "layered");

                // Split streams, exit codes, and tagged streaming callbacks.
                let seen: Arc<Mutex<Vec<(ExecStream, String)>>> = Arc::default();
                let sink = seen.clone();
                let on_event: OnStreamEventCallback = Arc::new(move |stream, line| {
                    let sink = sink.clone();
                    Box::pin(async move {
                        sink.lock().unwrap().push((stream, line));
                    })
                });
                let split = provider
                    .exec_streamed(
                        &handle,
                        vec![
                            "sh".into(),
                            "-c".into(),
                            "echo out; echo err 1>&2; exit 3".into(),
                        ],
                        HashMap::new(),
                        Some(on_event),
                    )
                    .await
                    .unwrap();
                assert_eq!(split.exit_code, 3);
                assert_eq!(split.stdout, "out\n");
                assert_eq!(split.stderr, "err\n");
                let seen = seen.lock().unwrap().clone();
                assert!(seen.contains(&(ExecStream::Stdout, "out".to_string())));
                assert!(seen.contains(&(ExecStream::Stderr, "err".to_string())));

                // Root exec.
                let whoami = provider
                    .exec_as_root(
                        &handle,
                        vec!["id".into(), "-u".into()],
                        HashMap::new(),
                        None,
                    )
                    .await
                    .unwrap();
                assert_eq!(whoami.stdout.trim(), "0");

                // Files: write (creating parents) with a mode, read back.
                let path = "/workspace/nested/dir/hello.txt";
                let payload = b"hello from the host\n\x00binary\xff";
                provider
                    .write_file(&handle, path, payload, 0o600)
                    .await
                    .unwrap();
                assert_eq!(provider.read_file(&handle, path).await.unwrap(), payload);
                let mode = sh(provider, &handle, &format!("stat -c %a {path}")).await;
                assert_eq!(mode.stdout.trim(), "600");
                match provider.read_file_bounded(&handle, path, 4).await {
                    Err(AgentError::Validation { message }) => {
                        assert!(message.contains(path), "{message}")
                    }
                    other => panic!("expected size-limit error, got {other:?}"),
                }
                assert_eq!(
                    provider
                        .read_file_bounded(&handle, path, payload.len() as u64)
                        .await
                        .unwrap(),
                    payload,
                    "a file exactly at the limit is returned whole"
                );
                // A multi-chunk file streams through the bounded reader intact.
                let big = sh(
                    provider,
                    &handle,
                    "head -c 1048576 /dev/urandom > /workspace/big.bin",
                )
                .await;
                assert_eq!(big.exit_code, 0, "{}", big.stderr);
                let whole = provider
                    .read_file(&handle, "/workspace/big.bin")
                    .await
                    .unwrap();
                let bounded = provider
                    .read_file_bounded(&handle, "/workspace/big.bin", 1_048_576)
                    .await
                    .unwrap();
                assert!(bounded == whole, "bounded stream matches the full read");
                match provider.read_file(&handle, "/workspace/missing").await {
                    Err(AgentError::SandboxExecFailed {
                        sandbox_id, reason, ..
                    }) => {
                        assert_eq!(sandbox_id, handle.sandbox_name);
                        assert!(reason.contains("/workspace/missing"), "{reason}");
                    }
                    other => panic!("expected read error, got {other:?}"),
                }

                // network_mode "none": no network device from the runtime
                // (the guest kernel's inert `dummy0` aside), no routes, and
                // no egress.
                let links = sh(provider, &handle, "ls /sys/class/net").await;
                assert!(
                    links
                        .stdout
                        .split_whitespace()
                        .all(|l| l == "lo" || l == "dummy0"),
                    "unexpected interfaces: {}",
                    links.stdout
                );
                let routes = sh(provider, &handle, ROUTE_COUNT).await;
                assert_eq!(routes.stdout.trim(), "0", "routing table must be empty");
                let egress = sh(provider, &handle, EGRESS_PROBE).await;
                assert_ne!(egress.exit_code, 0, "egress must fail: {}", egress.stdout);

                // Per-VM footprint as the runtime reports it.
                if let Ok(Some(vm)) = provider.lookup(&handle.sandbox_name).await {
                    if let Ok(metrics) = vm.metrics().await {
                        println!(
                            "vm memory: rss {} MiB, host-resident guest {:?} MiB, limit {} MiB",
                            metrics.memory_bytes / (1024 * 1024),
                            metrics
                                .memory_host_resident_bytes
                                .map(|b| b / (1024 * 1024)),
                            metrics.memory_limit_bytes / (1024 * 1024)
                        );
                    }
                }

                // Stop/start keeps the root disk.
                provider.stop(&handle).await.unwrap();
                assert!(!provider.is_alive(&handle).await.unwrap());
                let restarted = Instant::now();
                provider.start(&handle).await.unwrap();
                println!("restarted in {} ms", restarted.elapsed().as_millis());
                assert!(provider.is_alive(&handle).await.unwrap());
                assert_eq!(provider.read_file(&handle, path).await.unwrap(), payload);

                // Recovery by bare label finds the same VM.
                let label = handle
                    .sandbox_name
                    .strip_prefix(MSB_SANDBOX_NAME_PREFIX)
                    .unwrap()
                    .to_string();
                let recovered = provider.recover_by_name(&label).await.unwrap().unwrap();
                assert_eq!(recovered.sandbox_name, handle.sandbox_name);
                assert_eq!(recovered.backend, SandboxBackend::Microsandbox);
            }
        })
        .await;
    }

    #[test]
    fn workspace_has_entries_only_for_nonempty_directories() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!workspace_has_entries(dir.path()), "empty dir");
        assert!(!workspace_has_entries(&dir.path().join("missing")));
        let file = dir.path().join("f");
        std::fs::write(&file, b"x").unwrap();
        assert!(workspace_has_entries(dir.path()));
        assert!(!workspace_has_entries(&file), "a file is not a workspace");
    }

    #[tokio::test]
    async fn e2e_create_seeds_workspace_from_host_work_dir() {
        let Some(provider) = e2e_provider() else {
            return;
        };
        // A prepared workspace like a workflow's cloned repository: nested
        // files, an executable, and a symlink pointing outside the tree,
        // which must not be followed into the guest.
        let host = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("host-secret"), b"do-not-copy").unwrap();
        std::fs::create_dir_all(host.path().join("src/nested")).unwrap();
        std::fs::write(host.path().join("README.md"), b"seeded-readme\n").unwrap();
        std::fs::write(host.path().join("src/nested/lib.rs"), b"pub fn f() {}\n").unwrap();
        // Larger than one upload chunk, with a non-repeating pattern, so a
        // dropped or reordered chunk shows up as a content mismatch.
        let big: Vec<u8> = (0..(UPLOAD_CHUNK_BYTES * 3 + 4321) as u32)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8)
            .collect();
        std::fs::write(host.path().join("src/big.bin"), &big).unwrap();
        let script = host.path().join("run.sh");
        std::fs::write(&script, b"#!/bin/sh\necho ran\n").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::os::unix::fs::symlink(outside.path().join("host-secret"), host.path().join("leak"))
            .unwrap();

        let mut config = e2e_config("seed", "none");
        config.host_work_dir = host.path().to_path_buf();
        with_sandbox(&provider, config, |handle| {
            let provider = &provider;
            async move {
                let readme = provider
                    .read_file(&handle, &format!("{WORK_DIR}/README.md"))
                    .await
                    .unwrap();
                assert_eq!(readme, b"seeded-readme\n");
                let nested = provider
                    .read_file(&handle, &format!("{WORK_DIR}/src/nested/lib.rs"))
                    .await
                    .unwrap();
                assert_eq!(nested, b"pub fn f() {}\n");
                let copied = provider
                    .read_file(&handle, &format!("{WORK_DIR}/src/big.bin"))
                    .await
                    .unwrap();
                assert!(copied == big, "multi-chunk file copied byte-for-byte");
                let ran = sh(provider, &handle, &format!("cd {WORK_DIR} && ./run.sh")).await;
                assert_eq!(ran.exit_code, 0, "mode preserved: {}", ran.stderr);
                assert_eq!(ran.stdout.trim(), "ran");
                let leak = sh(provider, &handle, &format!("test -e {WORK_DIR}/leak")).await;
                assert_ne!(
                    leak.exit_code, 0,
                    "symlinks are not followed into the guest"
                );
            }
        })
        .await;
    }

    #[test]
    fn git_metadata_names_cover_host_folding_rules() {
        for name in [
            ".git",
            ".GIT",
            ".Git",
            ".git.",
            ".git ",
            ".git. .",
            "GIT~1",
            "git~1",
            ".g\u{200C}it",
            "\u{FEFF}.git",
            ".gi\u{206F}t",
        ] {
            assert!(
                is_git_metadata_name(name),
                "{name:?} must be treated as .git"
            );
        }
        for name in [".github", ".gitignore", "git", ".git-hooks", "x.git", ".gt"] {
            assert!(!is_git_metadata_name(name), "{name:?} is an ordinary entry");
        }
    }

    #[test]
    fn guest_entry_names_must_be_plain_components() {
        assert_eq!(guest_entry_name("/workspace/src/lib.rs"), Some("lib.rs"));
        assert_eq!(guest_entry_name("README.md"), Some("README.md"));
        assert_eq!(guest_entry_name("/workspace/.git"), Some(".git"));
        assert_eq!(guest_entry_name("/workspace/.."), None);
        assert_eq!(guest_entry_name("/workspace/."), None);
        assert_eq!(guest_entry_name("/workspace/"), None);
        assert_eq!(guest_entry_name("/workspace/a\0b"), None);
    }

    #[tokio::test]
    async fn e2e_sync_workspace_mirrors_guest_edits_to_host() {
        let Some(provider) = e2e_provider() else {
            return;
        };
        let host = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("target"), b"untouched").unwrap();
        std::fs::create_dir_all(host.path().join("src/nested")).unwrap();
        std::fs::create_dir_all(host.path().join("old-dir/inner")).unwrap();
        std::fs::write(host.path().join("README.md"), b"before\n").unwrap();
        std::fs::write(host.path().join("src/nested/lib.rs"), b"pub fn f() {}\n").unwrap();
        std::fs::write(host.path().join("old-dir/inner/x"), b"x").unwrap();
        // Never copied in, so never the agent's to remove.
        std::os::unix::fs::symlink(outside.path().join("target"), host.path().join("host-link"))
            .unwrap();
        // The host repository the server will run `git` in.
        std::fs::create_dir_all(host.path().join(".git/hooks")).unwrap();
        let host_git_config = b"[core]\n\trepositoryformatversion = 0\n".to_vec();
        std::fs::write(host.path().join(".git/config"), &host_git_config).unwrap();

        let mut config = e2e_config("sync", "none");
        config.host_work_dir = host.path().to_path_buf();
        let host_dir = host.path().to_path_buf();
        with_sandbox(&provider, config, |handle| {
            let provider = &provider;
            async move {
                // What an agent does: edit, add, delete, chmod, and write a
                // file larger than one stream chunk.
                let script = format!(
                    "cd {WORK_DIR} && printf 'after\\n' > README.md \
                     && mkdir -p src/new && printf 'added\\n' > src/new/added.txt \
                     && rm src/nested/lib.rs && rm -r old-dir \
                     && printf '#!/bin/sh\\n' > tool.sh && chmod 755 tool.sh \
                     && head -c 1000000 /dev/urandom > blob.bin \
                     && ln -s /etc/passwd guest-link \
                     && printf '[core]\\n\\tfsmonitor = ./tool.sh\\n' >> .git/config \
                     && mkdir -p .git/hooks && printf '#!/bin/sh\\n' > .git/hooks/post-checkout \
                     && mkdir -p .GIT && printf '[core]\\n\\tfsmonitor = ./tool.sh\\n' > .GIT/config \
                     && mkdir -p .Git/hooks && printf '#!/bin/sh\\n' > .Git/hooks/pre-commit \
                     && mkdir -p \"$(printf '.g\\342\\200\\214it')\" \
                     && mkdir -p vendor/dep/.git && printf 'x' > vendor/dep/.git/config \
                     && printf 'dep\\n' > vendor/dep/lib.txt"
                );
                let edit = sh(provider, &handle, &script).await;
                assert_eq!(edit.exit_code, 0, "{}", edit.stderr);
                let blob = provider
                    .read_file(&handle, &format!("{WORK_DIR}/blob.bin"))
                    .await
                    .unwrap();

                provider
                    .sync_workspace_to_host(&handle, &host_dir)
                    .await
                    .unwrap();

                assert_eq!(
                    std::fs::read(host_dir.join("README.md")).unwrap(),
                    b"after\n"
                );
                assert_eq!(
                    std::fs::read(host_dir.join("src/new/added.txt")).unwrap(),
                    b"added\n"
                );
                assert!(
                    !host_dir.join("src/nested/lib.rs").exists(),
                    "deletion mirrored"
                );
                assert!(host_dir.join("src/nested").is_dir());
                assert!(
                    !host_dir.join("old-dir").exists(),
                    "directory deletion mirrored"
                );
                {
                    use std::os::unix::fs::PermissionsExt;
                    let mode = std::fs::metadata(host_dir.join("tool.sh"))
                        .unwrap()
                        .permissions()
                        .mode();
                    assert_eq!(mode & 0o777, 0o755);
                }
                assert!(
                    std::fs::read(host_dir.join("blob.bin")).unwrap() == blob,
                    "multi-chunk file copied back byte-for-byte"
                );
                assert!(
                    std::fs::symlink_metadata(host_dir.join("guest-link")).is_err(),
                    "guest symlinks are not materialized on the host"
                );
                assert!(std::fs::symlink_metadata(host_dir.join("host-link"))
                    .unwrap()
                    .file_type()
                    .is_symlink());
                // Guest Git metadata never reaches the host repository.
                assert_eq!(
                    std::fs::read(host_dir.join(".git/config")).unwrap(),
                    host_git_config,
                    "host git config untouched"
                );
                assert!(!host_dir.join(".git/hooks/post-checkout").exists());
                assert!(!host_dir.join(".git/hooks/pre-commit").exists());
                let leaked: Vec<String> = std::fs::read_dir(&host_dir)
                    .unwrap()
                    .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                    .filter(|n| n != ".git" && is_git_metadata_name(n))
                    .collect();
                assert!(leaked.is_empty(), "case/Unicode variants of .git synced: {leaked:?}");
                assert!(
                    !host_dir.join("vendor/dep/.git").exists(),
                    "nested .git skipped"
                );
                assert_eq!(
                    std::fs::read(host_dir.join("vendor/dep/lib.txt")).unwrap(),
                    b"dep\n"
                );
            }
        })
        .await;
        assert_eq!(
            std::fs::read(outside.path().join("target")).unwrap(),
            b"untouched"
        );
    }

    #[tokio::test]
    async fn e2e_restricted_network_denies_public_egress() {
        let Some(provider) = e2e_provider() else {
            return;
        };
        with_sandbox(
            &provider,
            e2e_config("restricted", "restricted"),
            |handle| {
                let provider = &provider;
                async move {
                    // A routed interface exists (unlike "none"), but the
                    // host-side policy drops traffic to the public internet.
                    let routes = sh(provider, &handle, ROUTE_COUNT).await;
                    assert_ne!(
                        routes.stdout.trim(),
                        "0",
                        "restricted keeps a routed interface"
                    );
                    let egress = sh(provider, &handle, EGRESS_PROBE).await;
                    assert_ne!(
                        egress.exit_code, 0,
                        "egress must be denied: {}",
                        egress.stdout
                    );
                }
            },
        )
        .await;
    }

    #[tokio::test]
    async fn e2e_full_network_reaches_the_public_internet() {
        let Some(provider) = e2e_provider() else {
            return;
        };
        let online = tokio::time::timeout(
            Duration::from_secs(3),
            tokio::net::TcpStream::connect("1.1.1.1:80"),
        )
        .await
        .is_ok_and(|r| r.is_ok());
        if !online {
            println!("host has no internet access, skipping full-egress check");
            return;
        }
        with_sandbox(&provider, e2e_config("full", "full"), |handle| {
            let provider = &provider;
            async move {
                let egress = sh(provider, &handle, EGRESS_PROBE).await;
                assert_eq!(egress.exit_code, 0, "stderr: {}", egress.stderr);
                assert!(egress.stdout.contains("ip="), "{}", egress.stdout);
            }
        })
        .await;
    }
}
