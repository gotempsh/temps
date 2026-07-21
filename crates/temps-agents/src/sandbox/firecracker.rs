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
//! v1 sprint scope, deliberately deferred: the jailer (VMM runs as the
//! server's own user — still KVM-isolated), the ADR-013 egress credential
//! proxy, and snapshot-based pause.

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

    /// Actual on-disk bytes for a file (sparse-aware: counts allocated
    /// blocks, not the apparent length).
    fn disk_bytes(path: &Path) -> u64 {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(path)
            .map(|m| m.blocks() * 512)
            .unwrap_or(0)
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

        // Grow the per-VM disk to the requested size. The cache is sized to
        // the image content (its floor); we only ever grow, never shrink, so
        // the requested size is clamped up to that floor. `resize2fs` adds
        // only metadata for the new range — the extra space stays sparse
        // until the guest writes to it, so a big disk still costs image-size.
        let base_bytes = std::fs::metadata(&vm_rootfs)?.len();
        let requested_bytes = config
            .disk_size_mb
            .unwrap_or(self.config.default_disk_mb)
            .saturating_mul(1024 * 1024);
        if requested_bytes > base_bytes {
            if let Err(e) = self.grow_rootfs(&vm_rootfs, requested_bytes).await {
                let _ = std::fs::remove_dir_all(&vm_dir);
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

        // Networking: `"none"` boots without a NIC (stronger than Docker's
        // none — there is no device at all). `"restricted"` is meant to scope
        // egress to the credential proxy (ADR-013), which isn't built yet —
        // so it FAILS CLOSED to no-network rather than silently granting the
        // full-NAT egress it's asking to restrict. `"full"`/None get a TAP.
        // Guest addressing rides the kernel's built-in IP autoconfig.
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

/// Total apparent size of a directory tree (bytes), following the entries
/// as extracted. Used to size the cached ext4 to its image content.
fn dir_size(root: &Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                stack.push(entry.path());
            } else {
                total += meta.len();
            }
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider() -> FirecrackerSandboxProvider {
        let docker = Arc::new(bollard::Docker::connect_with_local_defaults().unwrap());
        FirecrackerSandboxProvider::new(
            FirecrackerSandboxConfig::from_data_dir(PathBuf::from("/nonexistent")),
            docker,
        )
    }

    #[test]
    fn resolve_name_prefers_override() {
        let p = provider();
        let mut config = SandboxCreateConfig {
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
}
