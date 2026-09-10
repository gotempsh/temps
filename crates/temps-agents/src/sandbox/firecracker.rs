// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Firecracker microVM sandbox backend (ADR-029).
//!
//! Each sandbox is a KVM microVM: pinned guest kernel, an ext4 rootfs
//! derived from the sandbox's Docker image (Docker is the image toolchain —
//! pull/export happen through bollard, ADR-029 §4), and `temps-vm-agent`
//! injected as PID 1 serving exec/fs RPCs over vsock (§5).
//!
//! On-disk layout under `<data_dir>/firecracker/` (provisioned by
//! `temps firecracker setup`):
//!   bin/{firecracker,jailer,temps-vm-agent}   pinned binaries
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
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use temps_vm_agent::{Request, Response, AGENT_PORT, MAX_FRAME_BYTES, WORK_DIR};

use super::{KillSignal, SandboxCreateConfig, SandboxExecResult, SandboxHandle, SandboxProvider};
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
const RPC_TIMEOUT: Duration = Duration::from_secs(300);
const SHUTDOWN_GRACE: Duration = Duration::from_secs(8);
const SNAPSHOT_EXTRACTION_TIMEOUT: Duration = Duration::from_secs(15 * 60);
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

    fn fc_root(&self) -> PathBuf {
        self.data_dir.join("firecracker")
    }
    fn firecracker_bin(&self) -> PathBuf {
        self.fc_root().join("bin/firecracker")
    }
    fn agent_bin(&self) -> PathBuf {
        self.fc_root().join("bin/temps-vm-agent")
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
            sandbox_id: name.to_string(),
            sandbox_name: name.to_string(),
            work_dir: PathBuf::from(WORK_DIR),
            backend: super::SandboxBackend::Firecracker,
            image,
        }
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
        let cache_key = image_id.replace(':', "-");
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
        let agent_src = self.config.agent_bin();
        if !agent_src.exists() {
            return Err(self.err(
                "-",
                format!(
                    "guest agent missing at {} — run `temps firecracker setup`",
                    agent_src.display()
                ),
            ));
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
            let mut archive = tar::Archive::new(std::io::BufReader::new(tar_file));
            for entry in archive.entries().map_err(|e| e.to_string())? {
                let mut entry = entry.map_err(|e| e.to_string())?;
                if matches!(
                    entry.header().entry_type(),
                    tar::EntryType::Regular
                        | tar::EntryType::Directory
                        | tar::EntryType::Symlink
                        | tar::EntryType::Link
                ) {
                    let _ = entry
                        .unpack_in(&rootfs_for_task)
                        .map_err(|e| e.to_string())?;
                }
            }
            let _ = std::fs::remove_file(&tar_path);
            for artifact in [".dockerenv", "etc/resolv.conf", "etc/hostname", "etc/hosts"] {
                let _ = std::fs::remove_file(rootfs_for_task.join(artifact));
            }
            let sbin = rootfs_for_task.join("sbin");
            std::fs::create_dir_all(&sbin).map_err(|e| e.to_string())?;
            std::fs::copy(&agent_src, sbin.join("temps-vm-agent")).map_err(|e| e.to_string())?;
            std::fs::create_dir_all(rootfs_for_task.join(&workdir_rel))
                .map_err(|e| e.to_string())?;
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

    // ── Vsock RPC client (hybrid Unix socket, one RPC per connection) ──

    async fn rpc(
        &self,
        name: &str,
        request: &Request,
        timeout: Duration,
    ) -> Result<Response, AgentError> {
        let sock = self.config.vm_dir(name).join("v.sock");
        let fut = async {
            let stream = UnixStream::connect(&sock).await?;
            let mut stream = BufReader::new(stream);
            stream
                .get_mut()
                .write_all(format!("CONNECT {}\n", AGENT_PORT).as_bytes())
                .await?;
            // Handshake ack: "OK <hostport>\n"
            let mut ack = Vec::new();
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
            let payload = serde_json::to_vec(request).map_err(std::io::Error::other)?;
            stream
                .get_mut()
                .write_all(&(payload.len() as u32).to_be_bytes())
                .await?;
            stream.get_mut().write_all(&payload).await?;
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
        // e2fsck return codes 0/1 (clean / errors-fixed) are both fine.
        let fsck = tokio::process::Command::new("e2fsck")
            .args(["-fy"])
            .arg(img)
            .output()
            .await?;
        if fsck.status.code().unwrap_or(8) > 1 {
            return Err(self.err(
                name,
                format!("e2fsck: {}", String::from_utf8_lossy(&fsck.stderr)),
            ));
        }
        let out = tokio::process::Command::new("resize2fs")
            .arg(img)
            .output()
            .await?;
        if !out.status.success() {
            return Err(self.err(
                name,
                format!("resize2fs: {}", String::from_utf8_lossy(&out.stderr)),
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
    ) -> Result<std::process::ExitStatus, SnapshotExtractorError> {
        command.kill_on_drop(true);
        let mut child = command.spawn().map_err(SnapshotExtractorError::Start)?;
        match tokio::time::timeout(timeout, child.wait()).await {
            Ok(Ok(status)) => Ok(status),
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

    fn sandboxed_debugfs_command(staging: &Path, source: &Path) -> tokio::process::Command {
        let mut command = tokio::process::Command::new("bwrap");
        command
            .args([
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
                "--bind",
            ])
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
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
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
            if !dump.success() {
                return Err(self.err(
                    sandbox_name,
                    format!("sandboxed debugfs snapshot export failed with status {dump}"),
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
        if self.vm_pid(&handle.sandbox_name).is_none() {
            return Ok(false);
        }
        Ok(matches!(
            self.rpc(&handle.sandbox_name, &Request::Ping, Duration::from_secs(3))
                .await,
            Ok(Response::Pong)
        ))
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

    fn supports_backend(&self, backend: super::SandboxBackend) -> bool {
        matches!(backend, super::SandboxBackend::Firecracker)
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
    use super::*;

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
            network_mode: None,
            env_vars: HashMap::new(),
            idle_timeout: Duration::from_secs(60),
            backend: None,
        };
        assert_eq!(p.resolve_name(&config), "temps-fcsandbox-abc123");
        config.container_name_override = None;
        assert_eq!(p.resolve_name(&config), "temps-fcsandbox-7");
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
