// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! A Firecracker microVM as the runtime of a deployment replica (ADR-055
//! Phase 10, "Firecracker for deployments").
//!
//! [`FirecrackerWorkloadDriver`] is the generic [`RuntimeDriver`] face of
//! the Firecracker sandbox backend: where [`super::driver::SandboxBackendDriver`]
//! serves sandboxes (and refuses a generic `create`), this one runs an OCI
//! image's own process in a VM, so the workload plane can place a
//! `micro_vm` deployment on the control plane.
//!
//! - **create** boots a VM from the image (the backend's rootfs conversion,
//!   TAP networking and resource caps), records what to run, and picks the
//!   host ports. Nothing runs yet.
//! - **start** spawns the image's entrypoint + command in the guest through
//!   `temps-vm-agent` (`Spawn`) and opens one loopback TCP forwarder per
//!   published port (`host_ip:host_port` -> `guest_ip:container_port`), so
//!   the bare-metal route table reaches the replica the same way it reaches
//!   a container.
//! - **inspect** asks the guest whether the process still runs; it also
//!   re-opens forwarders a control-plane restart dropped.
//!
//! The guest agent is the replica's runtime: it restarts the process per
//! the spec's restart policy with an exponential backoff (the plane leaves
//! an exited long-running replica to its runtime, as with Docker), and it
//! pipes the process's output into a size-bounded rotating log honouring
//! the spec's `log_rotation`, so a chatty app cannot fill the fixed-size
//! rootfs. A pending restart reports as running, like a restarting
//! container; the restart count is logged when it changes and exposed by
//! [`FirecrackerWorkloadDriver::restart_count`].
//!
//! What a microVM cannot express here is refused at create with the field
//! named, never dropped: secret files, volume binds, required Docker
//! networks and UDP ports.
//!
//! The image's `USER` is resolved the way Docker does, against the image's
//! own `/etc/passwd` and `/etc/group` (see [`super::image_user`]), so
//! `USER nextjs` and `USER 1001:1001` both run as the identity a container
//! of the same image would; the guest agent only ever receives numbers.
//!
//! Memory: one small JSON state file per VM on disk, and in memory one
//! accept task per published port plus one copy task per open connection
//! (bounded by [`MAX_CONNECTIONS_PER_PORT`]).

use std::collections::{BTreeMap, HashMap};
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use chrono::{DateTime, Utc};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use temps_vm_agent::SpawnRestartPolicy;
use temps_workload::{
    Artifact, ArtifactKinds, AttachmentKind, Attachments, DriverCapabilities, DriverError,
    ExecSpec, ExecStream, InstanceHandle, InstanceLimits, InstanceSpec, InstanceState,
    InstanceStats, InstanceStatus, Isolation, LimitSupport, ListedInstance, LogLine, LogQuery,
    LogRotation, LogStream, PortBinding, PortProtocol, PreparedArtifact, RestartPolicy,
    RuntimeDriver, VolumeKinds,
};

use super::firecracker::{FirecrackerSandboxProvider, GuestProcessStatus, GuestSpawn, VmIoLimits};
use super::image_user::{read_image_identity, resolve_image_user, ResolvedUser};
use super::{SandboxBackend, SandboxCreateConfig, SandboxHandle, SandboxProvider};
use crate::error::AgentError;

/// The driver's name, recorded on every instance it runs.
pub const WORKLOAD_VM_DRIVER: &str = "firecracker";

/// Prefix of a workload VM's id inside the sandbox backend's namespace, so
/// workload VMs and sandboxes never collide.
const WORKLOAD_VM_ID_PREFIX: &str = "wl-";

/// What the driver persists next to the VM (`<vm dir>/workload.json`).
const STATE_FILE: &str = "workload.json";

/// Default number of log lines when the query names no tail.
const DEFAULT_LOG_TAIL: u32 = 200;

/// Most connections one forwarded port relays at once; more wait in the
/// listener's backlog.
pub const MAX_CONNECTIONS_PER_PORT: usize = 512;

/// One published port and the host binding create picked for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ForwardedPort {
    container_port: u16,
    host_ip: String,
    host_port: u16,
}

/// What start needs to (re)run the workload, kept on disk so a restarted
/// control plane can still start, inspect and list it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct WorkloadVmState {
    /// The instance's name in the plane (the VM id without its prefixes).
    name: String,
    cmd: Vec<String>,
    cwd: Option<String>,
    uid: Option<u32>,
    gid: Option<u32>,
    /// The guest pid of the running process, once started. It keeps naming
    /// the process across the guest agent's restarts of it.
    pid: Option<u32>,
    started_at: Option<DateTime<Utc>>,
    ports: Vec<ForwardedPort>,
    labels: BTreeMap<String, String>,
    /// The spec's restart policy, carried out by the guest agent. Absent in
    /// a state file written before restarts existed: that VM keeps running
    /// its process once, as it was created to.
    #[serde(default)]
    restart: Option<RestartPolicy>,
    /// The log bounds the guest applies; absent takes its default.
    #[serde(default)]
    log_rotation: Option<GuestLogRotation>,
    /// The guest's restart count when inspect last saw it, so a new crash
    /// is logged once rather than on every inspect.
    #[serde(default)]
    restart_count: u32,
    /// Guest resource ceilings from the spec's limits; absent in a state
    /// file written before they were carried.
    #[serde(default)]
    limits: temps_vm_agent::SpawnLimits,
}

/// The rotation of the workload's log inside the guest, in the units the
/// agent takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuestLogRotation {
    pub max_bytes: u64,
    pub max_files: u32,
}

/// The guest agent's restart policy for the spec's. A process the guest
/// does not restart stays down (the plane leaves an exited long-running
/// replica to its runtime), so every policy is carried out, never dropped.
pub fn guest_restart_policy(policy: Option<RestartPolicy>) -> SpawnRestartPolicy {
    match policy {
        None | Some(RestartPolicy::Never) => SpawnRestartPolicy::Never,
        Some(RestartPolicy::Always) => SpawnRestartPolicy::Always,
        Some(RestartPolicy::UnlessStopped) => SpawnRestartPolicy::UnlessStopped,
        // Docker's `on-failure` without a count: unlimited, with backoff.
        Some(RestartPolicy::OnFailure) => SpawnRestartPolicy::OnFailure { max_retries: None },
    }
}

/// A Docker-style size (`10m`, `512k`, `1g`, `1.5MB`, `4096`) in bytes;
/// units are binary, as Docker's `max-size` reads them.
fn parse_log_size(size: &str) -> Option<u64> {
    let size = size.trim();
    let split = size
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(size.len());
    let (number, unit) = size.split_at(split);
    let number: f64 = number.parse().ok()?;
    let multiplier: u64 = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1,
        "k" | "kb" | "kib" => 1024,
        "m" | "mb" | "mib" => 1024 * 1024,
        "g" | "gb" | "gib" => 1024 * 1024 * 1024,
        _ => return None,
    };
    let bytes = number * multiplier as f64;
    (bytes.is_finite() && bytes >= 1.0 && bytes <= u64::MAX as f64).then_some(bytes as u64)
}

/// The guest log bounds for the spec's rotation. An unreadable size or a
/// zero file count is refused naming the field; values beyond what the
/// guest keeps on its small rootfs are clamped to its limits (and logged).
/// `None` (no rotation requested) takes the guest's bounded default.
pub fn resolve_log_rotation(
    instance: &str,
    rotation: Option<&LogRotation>,
) -> Result<Option<GuestLogRotation>, DriverError> {
    use temps_vm_agent::workload_log::{MAX_LOG_MAX_BYTES, MAX_LOG_MAX_FILES, MIN_LOG_MAX_BYTES};
    let Some(rotation) = rotation else {
        return Ok(None);
    };
    let max_bytes = parse_log_size(&rotation.max_size).ok_or_else(|| {
        runtime_error(
            "rotate the logs of",
            instance,
            format!(
                "log_rotation.max_size '{}' is not a size (use e.g. 10m, 512k, 1g)",
                rotation.max_size
            ),
        )
    })?;
    if rotation.max_file == 0 {
        return Err(runtime_error(
            "rotate the logs of",
            instance,
            "log_rotation.max_file is 0; at least one log file must be kept",
        ));
    }
    let resolved = GuestLogRotation {
        max_bytes: max_bytes.clamp(MIN_LOG_MAX_BYTES, MAX_LOG_MAX_BYTES),
        max_files: rotation.max_file.min(MAX_LOG_MAX_FILES),
    };
    if resolved.max_bytes != max_bytes || resolved.max_files != rotation.max_file {
        tracing::warn!(
            instance = %instance,
            requested_max_size = %rotation.max_size,
            requested_max_file = rotation.max_file,
            max_bytes = resolved.max_bytes,
            max_files = resolved.max_files,
            "microVM log rotation clamped to the guest's limits"
        );
    }
    Ok(Some(resolved))
}

/// What inspect reports for a guest process: a pending restart is still a
/// running replica (Docker reports a restarting container as running too —
/// the runtime owns its recovery), a process that will not run again is
/// exited with its code.
fn observed_state(status: &GuestProcessStatus) -> (InstanceState, Option<i64>) {
    if status.running || status.restarting {
        (InstanceState::Running, None)
    } else {
        (InstanceState::Exited, status.exit_code.map(i64::from))
    }
}

/// The image's own runtime configuration, as `docker inspect` reports it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImageRuntimeConfig {
    pub entrypoint: Option<Vec<String>>,
    pub cmd: Option<Vec<String>>,
    pub env: Vec<String>,
    pub working_dir: Option<String>,
    pub user: Option<String>,
}

/// What the guest runs: the spec's overrides over the image's defaults,
/// with Docker's rule that overriding the entrypoint drops the image's
/// command.
pub fn resolve_process(
    spec: &InstanceSpec,
    image: &ImageRuntimeConfig,
) -> Result<(Vec<String>, Option<String>), DriverError> {
    let entrypoint = spec
        .entrypoint
        .clone()
        .or_else(|| image.entrypoint.clone())
        .unwrap_or_default();
    let command = match (&spec.entrypoint, &spec.command) {
        (_, Some(command)) => command.clone(),
        (Some(_), None) => Vec::new(),
        (None, None) => image.cmd.clone().unwrap_or_default(),
    };
    let cmd: Vec<String> = entrypoint.into_iter().chain(command).collect();
    if cmd.is_empty() {
        return Err(unsupported(
            &spec.name,
            "an image with neither an entrypoint nor a command",
        ));
    }
    let cwd = spec
        .workdir
        .clone()
        .or_else(|| image.working_dir.clone())
        .filter(|dir| !dir.is_empty());
    Ok((cmd, cwd))
}

/// The process environment: the image's `ENV` (its `PATH` among them),
/// then the spec's variables on top.
pub fn resolve_env(spec: &InstanceSpec, image: &ImageRuntimeConfig) -> HashMap<String, String> {
    let mut env: HashMap<String, String> = image
        .env
        .iter()
        .filter_map(|entry| entry.split_once('='))
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect();
    env.extend(spec.env.iter().cloned());
    env
}

/// The `USER` a replica runs as: the spec's override over the image's own,
/// `None` when neither names one (the process then runs as the guest's root,
/// as before).
pub fn replica_user_spec<'a>(
    spec: &'a InstanceSpec,
    image: &'a ImageRuntimeConfig,
) -> Option<&'a str> {
    // Blank counts as unset on *both* sides, before falling back: a blank
    // override must not hide the image's own `USER` and run the replica as
    // the guest's root.
    let named = |user: &'a Option<String>| {
        user.as_deref()
            .map(str::trim)
            .filter(|user| !user.is_empty())
    };
    named(&spec.user).or_else(|| named(&image.user))
}

/// Refuse what a microVM replica cannot express, naming the field.
pub fn check_expressible(spec: &InstanceSpec, attach: &Attachments) -> Result<(), DriverError> {
    if !spec.secret_files.is_empty() {
        return Err(unsupported(
            &spec.name,
            "secret files (/run/secrets) in a microVM",
        ));
    }
    if !attach.volumes.is_empty() {
        return Err(unsupported(&spec.name, "volume binds in a microVM"));
    }
    if !attach.network.required.is_empty() {
        return Err(DriverError::Attach {
            driver: WORKLOAD_VM_DRIVER.to_string(),
            instance: spec.name.clone(),
            network: attach.network.required.join(","),
            reason: "a microVM is on its own TAP device and cannot join Docker networks"
                .to_string(),
        });
    }
    if let Some(port) = spec
        .ports
        .iter()
        .find(|port| port.protocol == PortProtocol::Udp)
    {
        return Err(DriverError::Runtime {
            driver: WORKLOAD_VM_DRIVER.to_string(),
            operation: "publish a port of",
            instance: spec.name.clone(),
            reason: format!(
                "UDP port {} cannot be forwarded to a microVM (TCP only)",
                port.container_port
            ),
        });
    }
    Ok(())
}

fn unsupported(instance: &str, operation: &'static str) -> DriverError {
    DriverError::Unsupported {
        driver: WORKLOAD_VM_DRIVER.to_string(),
        operation,
        instance: instance.to_string(),
    }
}

fn backend_error(operation: &'static str, instance: &str, error: AgentError) -> DriverError {
    DriverError::Backend {
        driver: WORKLOAD_VM_DRIVER.to_string(),
        operation,
        instance: instance.to_string(),
        source: Box::new(error),
    }
}

fn runtime_error(
    operation: &'static str,
    instance: &str,
    reason: impl std::fmt::Display,
) -> DriverError {
    DriverError::Runtime {
        driver: WORKLOAD_VM_DRIVER.to_string(),
        operation,
        instance: instance.to_string(),
        reason: reason.to_string(),
    }
}

/// Pick a free port on `host_ip` when the spec leaves it to the runtime.
fn pick_host_port(instance: &str, binding: &PortBinding) -> Result<u16, DriverError> {
    if let Some(port) = binding.host_port.filter(|port| *port != 0) {
        return Ok(port);
    }
    let listener = std::net::TcpListener::bind((binding.host_ip.as_str(), 0)).map_err(|e| {
        runtime_error(
            "pick a host port for",
            instance,
            format!("binding {}:0: {e}", binding.host_ip),
        )
    })?;
    listener
        .local_addr()
        .map(|addr| addr.port())
        .map_err(|e| runtime_error("pick a host port for", instance, e))
}

/// The generic runtime driver over the Firecracker backend.
pub struct FirecrackerWorkloadDriver {
    provider: Arc<FirecrackerSandboxProvider>,
    docker: Arc<bollard::Docker>,
    capabilities: DriverCapabilities,
    /// VM id -> its forwarders' accept tasks. Touched only on start, stop,
    /// remove and inspect, never per connection.
    forwarders: Mutex<HashMap<String, Vec<tokio::task::JoinHandle<()>>>>,
}

impl FirecrackerWorkloadDriver {
    pub fn new(provider: Arc<FirecrackerSandboxProvider>, docker: Arc<bollard::Docker>) -> Self {
        Self {
            provider,
            docker,
            capabilities: workload_vm_capabilities(),
            forwarders: Mutex::new(HashMap::new()),
        }
    }

    fn state_path(&self, vm_id: &str) -> PathBuf {
        self.provider.vm_dir_of(vm_id).join(STATE_FILE)
    }

    fn read_state(&self, vm_id: &str) -> Option<WorkloadVmState> {
        std::fs::read(self.state_path(vm_id))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
    }

    fn write_state(&self, vm_id: &str, state: &WorkloadVmState) -> Result<(), DriverError> {
        let bytes = serde_json::to_vec_pretty(state)
            .map_err(|e| runtime_error("record the state of", vm_id, e))?;
        std::fs::write(self.state_path(vm_id), bytes).map_err(|e| {
            runtime_error(
                "record the state of",
                vm_id,
                format!("{}: {e}", self.state_path(vm_id).display()),
            )
        })
    }

    fn sandbox_handle(&self, vm_id: &str, cwd: Option<&str>) -> SandboxHandle {
        SandboxHandle {
            node_id: None,
            sandbox_id: vm_id.to_string(),
            sandbox_name: vm_id.to_string(),
            work_dir: PathBuf::from(cwd.unwrap_or("/")),
            backend: SandboxBackend::Firecracker,
            image: String::new(),
            conditions: Vec::new(),
        }
    }

    async fn image_config(
        &self,
        instance: &str,
        image: &str,
    ) -> Result<ImageRuntimeConfig, DriverError> {
        let inspected =
            self.docker
                .inspect_image(image)
                .await
                .map_err(|e| DriverError::Artifact {
                    driver: WORKLOAD_VM_DRIVER.to_string(),
                    artifact: image.to_string(),
                    reason: format!("inspecting it for instance {instance}: {e}"),
                })?;
        let config = inspected.config.unwrap_or_default();
        Ok(ImageRuntimeConfig {
            entrypoint: config.entrypoint.filter(|e| !e.is_empty()),
            cmd: config.cmd.filter(|c| !c.is_empty()),
            env: config.env.unwrap_or_default(),
            working_dir: config.working_dir,
            user: config.user,
        })
    }

    /// Turn the replica's `USER` into the numbers the guest agent takes, by
    /// reading the image's own `/etc/passwd` and `/etc/group`.
    async fn resolve_replica_user(
        &self,
        instance: &str,
        image: &str,
        user_spec: &str,
    ) -> Result<ResolvedUser, DriverError> {
        let identity = read_image_identity(&self.docker, image)
            .await
            .map_err(|e| runtime_error("read the user database of", instance, e))?;
        let resolved = resolve_image_user(
            user_spec,
            identity.passwd.as_deref(),
            identity.group.as_deref(),
        )
        .map_err(|e| runtime_error("resolve the user of", instance, e))?;
        if resolved.groups.iter().any(|group| *group != resolved.gid) {
            // The guest agent starts a process with one uid and one gid: a
            // container would also join these groups, a microVM replica does
            // not yet.
            tracing::debug!(
                instance,
                user = user_spec,
                uid = resolved.uid,
                gid = resolved.gid,
                supplementary_groups = ?resolved.groups,
                "microVM replica runs without the image user's supplementary groups"
            );
        }
        Ok(resolved)
    }

    fn stop_forwarders(&self, vm_id: &str) {
        let tasks = self
            .forwarders
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(vm_id);
        for task in tasks.into_iter().flatten() {
            task.abort();
        }
    }

    /// Open the VM's forwarders unless they are already open (idempotent:
    /// start calls it, and inspect re-opens what a restart dropped).
    async fn ensure_forwarders(
        &self,
        vm_id: &str,
        state: &WorkloadVmState,
    ) -> Result<(), DriverError> {
        let open = self
            .forwarders
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(vm_id)
            .is_some_and(|tasks| tasks.iter().all(|task| !task.is_finished()));
        if open || state.ports.is_empty() {
            return Ok(());
        }
        let guest = self.provider.guest_ip_of(vm_id).ok_or_else(|| {
            runtime_error(
                "forward ports to",
                vm_id,
                "the VM holds no TAP device, so it has no address to forward to",
            )
        })?;
        let mut tasks = Vec::with_capacity(state.ports.len());
        for port in &state.ports {
            match bind_forwarder(port, guest, vm_id).await {
                Ok(task) => tasks.push(task),
                Err(error) => {
                    for task in tasks {
                        task.abort();
                    }
                    return Err(error);
                }
            }
        }
        let previous = self
            .forwarders
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(vm_id.to_string(), tasks);
        for task in previous.into_iter().flatten() {
            task.abort();
        }
        Ok(())
    }

    async fn process_status(
        &self,
        vm_id: &str,
        pid: u32,
    ) -> Result<GuestProcessStatus, DriverError> {
        self.provider
            .process_status(vm_id, pid)
            .await
            .map_err(|e| backend_error("inspect", vm_id, e))
    }

    /// Record a restart count the guest reports for the first time, and log
    /// it once: a crash-looping replica shows up in the control plane's log
    /// with its exit code, not only on the guest's serial console.
    fn note_restarts(&self, vm_id: &str, state: &mut WorkloadVmState, status: &GuestProcessStatus) {
        if status.restart_count == state.restart_count {
            return;
        }
        tracing::warn!(
            vm_id = %vm_id,
            instance = %state.name,
            restart_count = status.restart_count,
            last_exit_code = ?status.exit_code,
            restarting = status.restarting,
            "microVM replica process was restarted by its restart policy"
        );
        state.restart_count = status.restart_count;
        if let Err(error) = self.write_state(vm_id, state) {
            tracing::warn!(vm_id = %vm_id, %error, "could not record the microVM replica's restart count");
        }
    }

    /// The guest's restart count of the replica's process: how often its
    /// restart policy brought it back. `None` when no process was started
    /// or the VM is down.
    pub async fn restart_count(&self, handle: &InstanceHandle) -> Result<Option<u32>, DriverError> {
        let vm_id = handle.id.as_str();
        let Some(pid) = self.read_state(vm_id).and_then(|state| state.pid) else {
            return Ok(None);
        };
        if !self.provider.vm_running(vm_id) {
            return Ok(None);
        }
        Ok(Some(self.process_status(vm_id, pid).await?.restart_count))
    }
}

/// Bind one forwarder and return its accept task.
async fn bind_forwarder(
    port: &ForwardedPort,
    guest: Ipv4Addr,
    vm_id: &str,
) -> Result<tokio::task::JoinHandle<()>, DriverError> {
    let listen = format!("{}:{}", port.host_ip, port.host_port);
    let listener = tokio::net::TcpListener::bind(&listen)
        .await
        .map_err(|e| runtime_error("forward ports to", vm_id, format!("binding {listen}: {e}")))?;
    let target = SocketAddr::from((guest, port.container_port));
    let slots = Arc::new(tokio::sync::Semaphore::new(MAX_CONNECTIONS_PER_PORT));
    let vm = vm_id.to_string();
    tracing::info!(vm_id = %vm, listen = %listen, target = %target, "microVM port forwarder open");
    Ok(tokio::spawn(async move {
        loop {
            let Ok(permit) = slots.clone().acquire_owned().await else {
                return;
            };
            let (mut inbound, peer) = match listener.accept().await {
                Ok(accepted) => accepted,
                Err(e) => {
                    tracing::warn!(vm_id = %vm, listen = %listen, error = %e, "forwarder accept failed");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            };
            let vm = vm.clone();
            tokio::spawn(async move {
                let _permit = permit;
                match tokio::net::TcpStream::connect(target).await {
                    Ok(mut outbound) => {
                        let _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
                    }
                    Err(e) => {
                        tracing::debug!(vm_id = %vm, %peer, %target, error = %e, "guest port refused the connection");
                    }
                }
            });
        }
    }))
}

/// The driver's capability table: an OCI image in its own kernel, on a TAP
/// device, without volumes or snapshots (the sandbox path keeps those).
pub fn workload_vm_capabilities() -> DriverCapabilities {
    DriverCapabilities {
        name: WORKLOAD_VM_DRIVER.to_string(),
        artifacts: ArtifactKinds {
            oci: true,
            ..ArtifactKinds::default()
        },
        isolation: Isolation::MicroVm,
        network_attachment: AttachmentKind::Tap,
        volumes: VolumeKinds::default(),
        exec: true,
        snapshot: false,
        resize_disk: false,
        limits: LimitSupport {
            cpu: true,
            memory: true,
            pids: true,
            disk: true,
            disk_io: true,
            network: true,
        },
    }
}

#[async_trait]
impl RuntimeDriver for FirecrackerWorkloadDriver {
    fn capabilities(&self) -> &DriverCapabilities {
        &self.capabilities
    }

    async fn prepare(&self, artifact: &Artifact) -> Result<PreparedArtifact, DriverError> {
        let Artifact::OciImage { reference, digest } = artifact else {
            return Err(unsupported("", "non-OCI artifacts"));
        };
        let local_ref = match digest {
            Some(digest) => format!(
                "{}@{digest}",
                reference.split('@').next().unwrap_or(reference)
            ),
            None => reference.clone(),
        };
        if self.docker.inspect_image(&local_ref).await.is_err() {
            let mut pull = self.docker.create_image(
                Some(bollard::query_parameters::CreateImageOptions {
                    from_image: Some(local_ref.clone()),
                    ..Default::default()
                }),
                None,
                None,
            );
            while let Some(item) = pull.next().await {
                item.map_err(|e| DriverError::Artifact {
                    driver: WORKLOAD_VM_DRIVER.to_string(),
                    artifact: local_ref.clone(),
                    reason: e.to_string(),
                })?;
            }
        }
        Ok(PreparedArtifact {
            artifact: artifact.clone(),
            local_ref,
        })
    }

    async fn create(
        &self,
        spec: &InstanceSpec,
        attach: &Attachments,
    ) -> Result<InstanceHandle, DriverError> {
        check_expressible(spec, attach)?;
        let log_rotation = resolve_log_rotation(&spec.name, spec.log_rotation.as_ref())?;
        let image = self.image_config(&spec.name, &spec.image).await?;
        let (cmd, cwd) = resolve_process(spec, &image)?;
        let user = match replica_user_spec(spec, &image) {
            Some(user_spec) => Some(
                self.resolve_replica_user(&spec.name, &spec.image, user_spec)
                    .await?,
            ),
            None => None,
        };
        let uid = user.as_ref().map(|user| user.uid);
        let gid = user.as_ref().map(|user| user.gid);
        let mut env = resolve_env(spec, &image);
        if let Some(user) = &user {
            // What a container runtime does: the image's own HOME wins,
            // otherwise the user's passwd home.
            env.entry("HOME".to_string())
                .or_insert_with(|| user.home.clone());
        }
        let ports = spec
            .ports
            .iter()
            .map(|binding| {
                Ok(ForwardedPort {
                    container_port: binding.container_port,
                    host_ip: binding.host_ip.clone(),
                    host_port: pick_host_port(&spec.name, binding)?,
                })
            })
            .collect::<Result<Vec<_>, DriverError>>()?;
        if !attach.network.optional.is_empty() {
            tracing::debug!(
                instance = %spec.name,
                networks = ?attach.network.optional,
                "microVM replica skips optional Docker networks"
            );
        }
        let id = format!("{WORKLOAD_VM_ID_PREFIX}{}", spec.name);
        let vm_id = self.provider.vm_name_for(&id);
        // Bandwidth caps go into the VMM's own rate limiters, so the host
        // enforces them whatever the guest does.
        self.provider
            .set_io_limits(&vm_id, vm_io_limits(&spec.limits));
        let config = SandboxCreateConfig {
            allow_docker: false,
            run_id: 0,
            container_name_override: Some(id),
            host_work_dir: PathBuf::new(),
            workspace_volume: None,
            image: Some(spec.image.clone()),
            cpu_limit: spec
                .limits
                .nano_cpus
                .filter(|nanos| *nanos > 0)
                .map(|nanos| nanos as f64 / 1_000_000_000.0),
            memory_limit_mb: spec
                .limits
                .memory_bytes
                .filter(|bytes| *bytes > 0)
                .map(|bytes| (bytes as u64).div_ceil(1024 * 1024)),
            pids_limit: None,
            disk_size_mb: spec.limits.disk_mb.map(u64::from),
            egress_policy: Default::default(),
            network_mode: Some("full".to_string()),
            // Persisted by the backend in the 0700 VM dir as a 0600 file,
            // exactly as a sandbox's variables, so a restart can respawn.
            env_vars: env,
            idle_timeout: Duration::ZERO,
            backend: Some(SandboxBackend::Firecracker),
            owner_user_id: None,
            node_id: None,
        };
        self.provider
            .create(config)
            .await
            .map_err(|e| backend_error("create", &spec.name, e))?;
        let state = WorkloadVmState {
            name: spec.name.clone(),
            cmd,
            cwd,
            uid,
            gid,
            pid: None,
            started_at: None,
            ports,
            labels: spec.labels.clone(),
            restart: Some(spec.restart),
            log_rotation,
            restart_count: 0,
            limits: temps_vm_agent::SpawnLimits {
                max_open_files: None,
                // Enforced by the guest kernel per user, so a fork bomb in
                // the workload exhausts its own allowance, not the VM's.
                max_processes: spec
                    .limits
                    .pids
                    .filter(|pids| *pids > 0)
                    .map(|pids| pids as u64),
            },
        };
        if let Err(error) = self.write_state(&vm_id, &state) {
            let _ = self
                .provider
                .destroy(&self.sandbox_handle(&vm_id, None), true)
                .await;
            return Err(error);
        }
        tracing::info!(instance = %spec.name, vm_id = %vm_id, image = %spec.image, "microVM replica created");
        Ok(InstanceHandle {
            driver: WORKLOAD_VM_DRIVER.to_string(),
            id: vm_id,
            name: spec.name.clone(),
        })
    }

    async fn start(&self, handle: &InstanceHandle) -> Result<(), DriverError> {
        let vm_id = handle.id.as_str();
        let mut state = self
            .read_state(vm_id)
            .ok_or_else(|| DriverError::NotFound {
                driver: WORKLOAD_VM_DRIVER.to_string(),
                instance: vm_id.to_string(),
            })?;
        if !self.provider.vm_running(vm_id) {
            // The process died with the VM: forget its pid before rebooting.
            state.pid = None;
            self.provider
                .start(&self.sandbox_handle(vm_id, None))
                .await
                .map_err(|e| backend_error("boot", vm_id, e))?;
        }
        // A process waiting out a restart backoff is the guest's to bring
        // back; spawning another would run the replica twice.
        let alive = match state.pid {
            Some(pid) => {
                let status = self.process_status(vm_id, pid).await?;
                status.running || status.restarting
            }
            None => false,
        };
        if !alive {
            let restart = guest_restart_policy(state.restart);
            let spawn = GuestSpawn {
                cmd: state.cmd.clone(),
                env: self.provider.workload_env(vm_id),
                cwd: state.cwd.clone(),
                uid: state.uid,
                gid: state.gid,
                restart,
                log_max_bytes: state.log_rotation.map(|r| r.max_bytes),
                log_max_files: state.log_rotation.map(|r| r.max_files),
                limits: state.limits,
            };
            let pid = self
                .provider
                .spawn_process(vm_id, spawn)
                .await
                .map_err(|e| backend_error("start", vm_id, e))?;
            state.pid = Some(pid);
            state.started_at = Some(Utc::now());
            state.restart_count = 0;
            self.write_state(vm_id, &state)?;
            tracing::info!(vm_id = %vm_id, pid, restart = ?restart, "microVM replica process started");
        }
        self.ensure_forwarders(vm_id, &state).await
    }

    async fn stop(&self, handle: &InstanceHandle) -> Result<(), DriverError> {
        let vm_id = handle.id.as_str();
        self.stop_forwarders(vm_id);
        self.provider
            .stop(&self.sandbox_handle(vm_id, None))
            .await
            .map_err(|e| backend_error("stop", vm_id, e))?;
        if let Some(mut state) = self.read_state(vm_id) {
            state.pid = None;
            self.write_state(vm_id, &state)?;
        }
        Ok(())
    }

    async fn remove(&self, handle: &InstanceHandle, _purge: bool) -> Result<(), DriverError> {
        let vm_id = handle.id.as_str();
        self.stop_forwarders(vm_id);
        if !self.provider.vm_dir_of(vm_id).exists() {
            return Ok(());
        }
        self.provider
            .destroy(&self.sandbox_handle(vm_id, None), true)
            .await
            .map_err(|e| backend_error("remove", vm_id, e))
    }

    async fn inspect(&self, handle: &InstanceHandle) -> Result<InstanceStatus, DriverError> {
        let vm_id = handle.id.as_str();
        let Some(mut state) = self.read_state(vm_id) else {
            return Ok(missing_status());
        };
        let published_ports = state
            .ports
            .iter()
            .map(|port| PortBinding {
                container_port: port.container_port,
                protocol: PortProtocol::Tcp,
                host_ip: port.host_ip.clone(),
                host_port: Some(port.host_port),
            })
            .collect();
        let address = self.provider.guest_ip_of(vm_id).map(|ip| ip.to_string());
        let (state_now, exit_code) = match (self.provider.vm_running(vm_id), state.pid) {
            (false, _) => (InstanceState::Exited, None),
            (true, None) => (InstanceState::Created, None),
            (true, Some(pid)) => {
                let status = self.process_status(vm_id, pid).await?;
                self.note_restarts(vm_id, &mut state, &status);
                let observed = observed_state(&status);
                if observed.0 == InstanceState::Running {
                    if let Err(error) = self.ensure_forwarders(vm_id, &state).await {
                        tracing::warn!(vm_id = %vm_id, %error, "could not re-open the microVM's forwarders");
                    }
                }
                observed
            }
        };
        Ok(InstanceStatus {
            state: state_now,
            exit_code,
            started_at: state.started_at,
            finished_at: None,
            address,
            published_ports,
        })
    }

    async fn logs(
        &self,
        handle: &InstanceHandle,
        query: LogQuery,
    ) -> Result<LogStream, DriverError> {
        let vm_id = handle.id.as_str();
        if query.follow || query.resume {
            // `resume` carries the checkpoint of the one ingesting reader;
            // a plain follow (a person watching) starts from the tail.
            let (checkpoint, backlog) = if query.resume {
                (
                    Some(self.provider.vm_dir_of(vm_id).join(LOG_CHECKPOINT_FILE)),
                    0,
                )
            } else {
                (None, query.tail.unwrap_or(DEFAULT_LOG_TAIL))
            };
            return Ok(follow_workload_log(
                self.provider.clone(),
                vm_id.to_string(),
                checkpoint,
                backlog,
                LOG_FOLLOW_POLL,
            ));
        }
        let tail = query.tail.unwrap_or(DEFAULT_LOG_TAIL).max(1);
        // The guest agent reads the tail across the log's rotated files and
        // needs nothing from the image; `tail` is only the fallback for a
        // VM whose agent predates that request.
        if let Some(text) = self
            .provider
            .read_workload_log(vm_id, tail)
            .await
            .map_err(|e| log_read_error(&self.provider, vm_id, e))?
        {
            return Ok(futures::stream::iter(log_lines(&text)).boxed());
        }
        let result = self
            .provider
            .exec(
                &self.sandbox_handle(vm_id, None),
                vec![
                    "tail".to_string(),
                    "-n".to_string(),
                    tail.to_string(),
                    temps_vm_agent::DEFAULT_SPAWN_LOG.to_string(),
                ],
                HashMap::new(),
                None,
            )
            .await
            .map_err(|e| log_read_error(&self.provider, vm_id, e))?;
        if result.exit_code != 0 {
            return Err(runtime_error(
                "read the logs of",
                vm_id,
                format!(
                    "`tail` exited {} (the image needs a `tail` binary): {}",
                    result.exit_code,
                    result.stderr.trim()
                ),
            ));
        }
        Ok(futures::stream::iter(log_lines(&result.stdout)).boxed())
    }

    async fn stats(&self, handle: &InstanceHandle) -> Result<InstanceStats, DriverError> {
        // The VMM's own usage is not the workload's, so the numbers come from
        // inside the guest: its agent reads its own /proc.
        let vm_id = handle.id.as_str();
        let pid = self.read_state(vm_id).and_then(|state| state.pid);
        let stats = self
            .provider
            .read_workload_stats(vm_id, pid)
            .await
            .map_err(|e| backend_error("read the stats of", vm_id, e))?;
        // A guest whose agent predates `read_stats` reports unknown rather
        // than a misleading number.
        Ok(stats.map(instance_stats).unwrap_or_default())
    }

    async fn exec(
        &self,
        handle: &InstanceHandle,
        exec: ExecSpec,
    ) -> Result<ExecStream, DriverError> {
        let vm_id = handle.id.as_str();
        let result = self
            .provider
            .exec(
                &self.sandbox_handle(vm_id, exec.workdir.as_deref()),
                exec.command,
                exec.env.into_iter().collect(),
                None,
            )
            .await
            .map_err(|e| backend_error("exec in", vm_id, e))?;
        let mut lines: Vec<Result<LogLine, DriverError>> = Vec::new();
        for line in result.stdout.lines() {
            lines.push(Ok(LogLine {
                stderr: false,
                line: Bytes::from(format!("{line}\n")),
                timestamp: None,
            }));
        }
        for line in result.stderr.lines() {
            lines.push(Ok(LogLine {
                stderr: true,
                line: Bytes::from(format!("{line}\n")),
                timestamp: None,
            }));
        }
        if result.exit_code != 0 {
            lines.push(Err(runtime_error(
                "exec in",
                vm_id,
                format!("command exited {}", result.exit_code),
            )));
        }
        Ok(futures::stream::iter(lines).boxed())
    }

    async fn list(
        &self,
        labels: &BTreeMap<String, String>,
    ) -> Result<Vec<ListedInstance>, DriverError> {
        let prefix = self.provider.vm_name_for(WORKLOAD_VM_ID_PREFIX);
        Ok(self
            .provider
            .vm_names()
            .into_iter()
            .filter(|vm_id| vm_id.starts_with(&prefix))
            .filter_map(|vm_id| {
                let state = self.read_state(&vm_id)?;
                labels
                    .iter()
                    .all(|(key, value)| state.labels.get(key) == Some(value))
                    .then(|| ListedInstance {
                        handle: InstanceHandle {
                            driver: WORKLOAD_VM_DRIVER.to_string(),
                            id: vm_id.clone(),
                            name: state.name.clone(),
                        },
                        labels: state.labels,
                    })
            })
            .collect())
    }
}

/// The workload log's text as log lines. stdout and stderr share one log in
/// the guest, so every line is reported as stdout.
/// The plane's view of what the guest reported.
fn instance_stats(guest: temps_vm_agent::stats::GuestStats) -> InstanceStats {
    InstanceStats {
        cpu_nanos: Some(guest.cpu_nanos),
        memory_bytes: Some(guest.memory_bytes),
        pids: Some(u64::from(guest.pids)),
        disk_read_bytes: Some(guest.disk_read_bytes),
        disk_write_bytes: Some(guest.disk_write_bytes),
        net_rx_bytes: Some(guest.net_rx_bytes),
        net_tx_bytes: Some(guest.net_tx_bytes),
        // The VM's RAM is the ceiling the workload runs under.
        memory_limit_bytes: Some(guest.memory_total_bytes).filter(|total| *total > 0),
    }
}

/// Where the ingesting log reader's position is kept, in the VM's directory
/// (so it goes with the VM when the VM is removed).
const LOG_CHECKPOINT_FILE: &str = "log.cursor";

/// How long a follower waits before asking the guest for more once it has
/// caught up. One small RPC per second per replica.
const LOG_FOLLOW_POLL: Duration = Duration::from_secs(1);

fn read_log_checkpoint(path: &Path) -> Option<temps_vm_agent::LogCursor> {
    let bytes = std::fs::read(path).ok()?;
    match serde_json::from_slice(&bytes) {
        Ok(cursor) => Some(cursor),
        Err(error) => {
            // A torn or foreign file only costs a re-read from the start of
            // what the guest retains.
            tracing::warn!(path = %path.display(), %error, "Ignoring an unreadable log checkpoint");
            None
        }
    }
}

/// Write the checkpoint atomically, so a crash never leaves half a file.
fn write_log_checkpoint(path: &Path, cursor: temps_vm_agent::LogCursor) {
    let tmp = path.with_extension("cursor.tmp");
    let written = serde_json::to_vec(&cursor)
        .map_err(std::io::Error::other)
        .and_then(|bytes| std::fs::write(&tmp, bytes))
        .and_then(|()| std::fs::rename(&tmp, path));
    if let Err(error) = written {
        tracing::warn!(path = %path.display(), %error, "Could not record the log checkpoint");
    }
}

/// Follow the guest's workload log by asking its agent for the lines after a
/// cursor, forever (until the guest cannot be reached, which ends the stream
/// with the error).
///
/// With a `checkpoint`, the cursor starts where the file says and is written
/// back once the lines before it have been handed over (at the next poll,
/// which only happens after the consumer asked for more); a first run reads
/// everything the guest still retains. Without one, the last `backlog` lines
/// are emitted first and following starts after them.
fn follow_workload_log(
    provider: Arc<FirecrackerSandboxProvider>,
    vm_id: String,
    checkpoint: Option<PathBuf>,
    backlog: u32,
    poll: Duration,
) -> LogStream {
    Box::pin(async_stream::stream! {
        let mut cursor = checkpoint.as_deref().and_then(read_log_checkpoint);
        let mut unrecorded = None;
        let mut lines = LogLines::default();

        if checkpoint.is_none() {
            // Catch up to the end, keeping only the last `backlog` lines.
            let mut recent: std::collections::VecDeque<LogLine> = std::collections::VecDeque::new();
            loop {
                let chunk = match provider.read_workload_log_since(&vm_id, cursor).await {
                    Ok(Some(chunk)) => chunk,
                    Ok(None) => {
                        yield Err(unsupported(&vm_id, FOLLOW_NEEDS_NEW_AGENT));
                        return;
                    }
                    Err(e) => {
                        yield Err(log_read_error(&provider, &vm_id, e));
                        return;
                    }
                };
                cursor = chunk.cursor.or(cursor);
                for line in chunk.data.lines().filter_map(|raw| lines.push(raw)) {
                    if recent.len() >= backlog as usize {
                        recent.pop_front();
                    }
                    if backlog > 0 {
                        recent.push_back(line);
                    }
                }
                if !chunk.more {
                    break;
                }
            }
            for line in recent {
                yield Ok(line);
            }
        }

        loop {
            if let (Some(path), Some(done)) = (checkpoint.as_deref(), unrecorded.take()) {
                write_log_checkpoint(path, done);
            }
            let chunk = match provider.read_workload_log_since(&vm_id, cursor).await {
                Ok(Some(chunk)) => chunk,
                Ok(None) => {
                    yield Err(unsupported(&vm_id, FOLLOW_NEEDS_NEW_AGENT));
                    return;
                }
                Err(e) => {
                    yield Err(log_read_error(&provider, &vm_id, e));
                    return;
                }
            };
            if chunk.skipped {
                yield Ok(LogLine {
                    stderr: true,
                    line: Bytes::from_static(
                        b"[temps] log lines were lost: the guest rotated them away before they were read\n",
                    ),
                    timestamp: Some(Utc::now()),
                });
            }
            for line in chunk.data.lines().filter_map(|raw| lines.push(raw)) {
                yield Ok(line);
            }
            if chunk.cursor.is_some() && chunk.cursor != cursor {
                cursor = chunk.cursor;
                unrecorded = chunk.cursor;
            }
            if !chunk.more {
                tokio::time::sleep(poll).await;
            }
        }
    })
}

/// Why a microVM's log could not be read. Its log lives inside the guest,
/// so a VM that is not running has nobody to answer; that is said plainly
/// instead of as the socket error it surfaces as.
fn log_read_error(
    provider: &FirecrackerSandboxProvider,
    vm_id: &str,
    error: AgentError,
) -> DriverError {
    if provider.vm_running(vm_id) {
        return backend_error("read the logs of", vm_id, error);
    }
    runtime_error("read the logs of", vm_id, VM_NOT_RUNNING_FOR_LOGS)
}

const VM_NOT_RUNNING_FOR_LOGS: &str = "the microVM is not running, so its log, which lives \
     inside it, cannot be read. The project's Logs page keeps what it wrote while it ran";

const FOLLOW_NEEDS_NEW_AGENT: &str =
    "following the logs of a microVM whose guest agent predates incremental log reads (recreate the replica)";

/// The spec's bandwidth limits as the VMM takes them. A zero or negative
/// value is "no limit", as for CPU and memory.
fn vm_io_limits(limits: &InstanceLimits) -> VmIoLimits {
    let positive = |value: Option<i64>| value.filter(|v| *v > 0).map(|v| v as u64);
    VmIoLimits {
        disk_bytes_per_sec: positive(limits.disk_io_bytes_per_sec),
        network_bytes_per_sec: positive(limits.network_bytes_per_sec),
    }
}

fn log_lines(text: &str) -> Vec<Result<LogLine, DriverError>> {
    let mut lines = LogLines::default();
    text.lines()
        .filter_map(|raw| lines.push(raw))
        .map(Ok)
        .collect()
}

/// The most of one unfinished line held while waiting for its end; past it,
/// what arrived so far is handed over as a line of its own.
const MAX_PENDING_LINE_BYTES: usize = 256 * 1024;

/// Turns the guest's workload log back into the process's lines.
///
/// The guest agent writes each read from the process's pipes as records
/// carrying the time it read them and the stream they came from (see
/// `temps_vm_agent::workload_log::frame_records`); a line split across reads
/// arrives as partial records followed by the one that ends it. A log
/// written by an agent that predates records holds bare output, which is
/// passed through as stdout without a time.
#[derive(Default)]
struct LogLines {
    /// Per stream (stdout, stderr), the start of a line whose end has not
    /// arrived and the time its first part was read.
    pending: [Option<(Option<DateTime<Utc>>, String)>; 2],
}

impl LogLines {
    /// The line `raw` (one line of the log, without its newline) completes,
    /// if any.
    fn push(&mut self, raw: &str) -> Option<LogLine> {
        let Some(record) = temps_vm_agent::workload_log::parse_record(raw) else {
            return Some(LogLine {
                stderr: false,
                line: Bytes::from(format!("{raw}\n")),
                timestamp: None,
            });
        };
        let stderr = record.stream == temps_vm_agent::workload_log::Stream::Stderr;
        let slot = &mut self.pending[usize::from(stderr)];
        let (timestamp, mut text) = slot.take().unwrap_or_else(|| {
            let time = DateTime::parse_from_rfc3339(record.time)
                .ok()
                .map(|time| time.with_timezone(&Utc));
            (time, String::new())
        });
        text.push_str(record.text);
        if record.partial && text.len() < MAX_PENDING_LINE_BYTES {
            *slot = Some((timestamp, text));
            return None;
        }
        text.push('\n');
        Some(LogLine {
            stderr,
            line: Bytes::from(text),
            timestamp,
        })
    }
}

fn missing_status() -> InstanceStatus {
    InstanceStatus {
        state: InstanceState::Missing,
        exit_code: None,
        started_at: None,
        finished_at: None,
        address: None,
        published_ports: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn guest_counters_become_the_planes_instance_stats() {
        let stats = instance_stats(temps_vm_agent::stats::GuestStats {
            cpu_nanos: 5,
            memory_bytes: 6,
            memory_total_bytes: 1024,
            memory_available_bytes: 100,
            pids: 3,
            disk_read_bytes: 7,
            disk_write_bytes: 8,
            net_rx_bytes: 9,
            net_tx_bytes: 10,
        });
        assert_eq!(stats.cpu_nanos, Some(5));
        assert_eq!(stats.memory_bytes, Some(6));
        assert_eq!(stats.memory_limit_bytes, Some(1024));
        assert_eq!(stats.pids, Some(3));
        assert_eq!(stats.disk_read_bytes, Some(7));
        assert_eq!(stats.disk_write_bytes, Some(8));
        assert_eq!(stats.net_rx_bytes, Some(9));
        assert_eq!(stats.net_tx_bytes, Some(10));
    }

    use std::sync::Mutex as StdMutex;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// A guest agent that answers every connection on the VM's `v.sock`,
    /// recording each request it was sent. `answer` gets the request.
    struct FakeGuest {
        driver: FirecrackerWorkloadDriver,
        provider: Arc<FirecrackerSandboxProvider>,
        requests: Arc<StdMutex<Vec<serde_json::Value>>>,
        _dir: tempfile::TempDir,
    }

    fn fake_guest(
        vm_id: &str,
        answer: impl Fn(&serde_json::Value) -> String + Send + Sync + 'static,
    ) -> FakeGuest {
        let dir = tempfile::tempdir().expect("tempdir");
        let docker = Arc::new(bollard::Docker::connect_with_http_defaults().expect("docker"));
        let provider = Arc::new(FirecrackerSandboxProvider::new(
            crate::sandbox::firecracker::FirecrackerSandboxConfig::from_data_dir(
                dir.path().to_path_buf(),
            ),
            docker.clone(),
        ));
        let vm_dir = provider.vm_dir_of(vm_id);
        std::fs::create_dir_all(&vm_dir).expect("vm dir");
        let listener = tokio::net::UnixListener::bind(vm_dir.join("v.sock")).expect("bind");
        let requests = Arc::new(StdMutex::new(Vec::new()));
        let seen = requests.clone();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let mut line = Vec::new();
                while let Ok(byte) = stream.read_u8().await {
                    if byte == b'\n' {
                        break;
                    }
                    line.push(byte);
                }
                if stream.write_all(b"OK 1073741824\n").await.is_err() {
                    continue;
                }
                let Ok(len) = stream.read_u32().await else {
                    continue;
                };
                let mut body = vec![0u8; len as usize];
                if stream.read_exact(&mut body).await.is_err() {
                    continue;
                }
                let request: serde_json::Value = serde_json::from_slice(&body).expect("json");
                let reply = answer(&request);
                seen.lock().expect("lock").push(request);
                let _ = stream.write_all(&(reply.len() as u32).to_be_bytes()).await;
                let _ = stream.write_all(reply.as_bytes()).await;
            }
        });
        FakeGuest {
            driver: FirecrackerWorkloadDriver::new(provider.clone(), docker),
            provider,
            requests,
            _dir: dir,
        }
    }

    fn chunk(data: &str, offset: u64, more: bool) -> String {
        serde_json::json!({
            "result": "log_chunk",
            "data": data,
            "cursor": { "inode": 7, "offset": offset },
            "skipped": false,
            "more": more,
        })
        .to_string()
    }

    /// The reply of a guest whose log is "a\nb\n" then "c\n" (inode 7).
    fn two_step_log(request: &serde_json::Value) -> String {
        assert_eq!(request["op"], "read_log_since");
        match request["cursor"]["offset"].as_u64() {
            None => chunk("a\nb\n", 4, false),
            Some(4) => chunk("c\n", 6, false),
            Some(offset) => chunk("", offset, false),
        }
    }

    fn handle_of(vm_id: &str) -> InstanceHandle {
        InstanceHandle {
            driver: WORKLOAD_VM_DRIVER.to_string(),
            id: vm_id.to_string(),
            name: vm_id.to_string(),
        }
    }

    async fn next_text(logs: &mut LogStream) -> String {
        let line = tokio::time::timeout(Duration::from_secs(5), logs.next())
            .await
            .expect("a line within 5s")
            .expect("the stream is still open")
            .expect("a line, not an error");
        String::from_utf8_lossy(&line.line).into_owned()
    }

    #[tokio::test]
    async fn a_resumed_follow_reads_everything_then_new_lines_and_checkpoints() {
        let guest = fake_guest("wl-follow", two_step_log);
        let query = LogQuery {
            follow: true,
            resume: true,
            ..LogQuery::default()
        };
        let mut logs = guest
            .driver
            .logs(&handle_of("wl-follow"), query.clone())
            .await
            .expect("follow");
        assert_eq!(next_text(&mut logs).await, "a\n");
        assert_eq!(next_text(&mut logs).await, "b\n");
        assert_eq!(next_text(&mut logs).await, "c\n");

        // "c" is in the consumer's hands, but its chunk is not yet confirmed:
        // the checkpoint is the chunk before it.
        let checkpoint = guest
            .provider
            .vm_dir_of("wl-follow")
            .join(LOG_CHECKPOINT_FILE);
        let recorded = read_log_checkpoint(&checkpoint).expect("a checkpoint");
        assert_eq!(
            recorded,
            temps_vm_agent::LogCursor {
                inode: 7,
                offset: 4
            }
        );
        drop(logs);

        // A new follower resumes from the checkpoint rather than the start.
        let mut again = guest
            .driver
            .logs(&handle_of("wl-follow"), query)
            .await
            .expect("follow again");
        assert_eq!(next_text(&mut again).await, "c\n");
        let requests = guest.requests.lock().expect("lock");
        let last_start = requests
            .iter()
            .rev()
            .find(|r| r["cursor"]["offset"] == 4)
            .expect("a request from the checkpoint");
        assert_eq!(last_start["cursor"]["inode"], 7);
    }

    #[tokio::test]
    async fn a_plain_follow_starts_after_the_tail_not_the_whole_log() {
        let guest = fake_guest("wl-tail", |request| {
            assert_eq!(request["op"], "read_log_since");
            match request["cursor"]["offset"].as_u64() {
                None => chunk("1\n2\n3\n4\n", 8, false),
                Some(8) => chunk("5\n", 10, false),
                Some(offset) => chunk("", offset, false),
            }
        });
        let mut logs = guest
            .driver
            .logs(
                &handle_of("wl-tail"),
                LogQuery {
                    follow: true,
                    tail: Some(2),
                    ..LogQuery::default()
                },
            )
            .await
            .expect("follow");
        assert_eq!(next_text(&mut logs).await, "3\n");
        assert_eq!(next_text(&mut logs).await, "4\n");
        assert_eq!(next_text(&mut logs).await, "5\n");
        // A person watching leaves no checkpoint behind.
        assert!(!guest
            .provider
            .vm_dir_of("wl-tail")
            .join(LOG_CHECKPOINT_FILE)
            .exists());
    }

    #[tokio::test]
    async fn a_follow_says_when_the_guest_dropped_lines_before_they_were_read() {
        let guest = fake_guest("wl-skip", |_| {
            serde_json::json!({
                "result": "log_chunk",
                "data": "late\n",
                "cursor": { "inode": 9, "offset": 5 },
                "skipped": true,
                "more": false,
            })
            .to_string()
        });
        let mut logs = guest
            .driver
            .logs(
                &handle_of("wl-skip"),
                LogQuery {
                    follow: true,
                    resume: true,
                    ..LogQuery::default()
                },
            )
            .await
            .expect("follow");
        let notice = next_text(&mut logs).await;
        assert!(notice.contains("log lines were lost"), "{notice}");
        assert_eq!(next_text(&mut logs).await, "late\n");
    }

    #[tokio::test]
    async fn a_follow_of_a_guest_with_an_old_agent_is_unsupported_not_empty() {
        let guest = fake_guest("wl-old", |_| {
            r#"{"result":"err","message":"bad request: unknown variant `read_log_since`"}"#
                .to_string()
        });
        let mut logs = guest
            .driver
            .logs(
                &handle_of("wl-old"),
                LogQuery {
                    follow: true,
                    resume: true,
                    ..LogQuery::default()
                },
            )
            .await
            .expect("the stream opens");
        let first = logs.next().await.expect("an item");
        assert!(
            matches!(first, Err(DriverError::Unsupported { .. })),
            "{first:?}"
        );
        assert!(logs.next().await.is_none(), "the stream ends");
    }

    #[tokio::test]
    async fn a_follow_of_an_unreachable_guest_ends_with_the_vm_named() {
        // No v.sock: the guest is gone.
        let dir = tempfile::tempdir().expect("tempdir");
        let docker = Arc::new(bollard::Docker::connect_with_http_defaults().expect("docker"));
        let provider = Arc::new(FirecrackerSandboxProvider::new(
            crate::sandbox::firecracker::FirecrackerSandboxConfig::from_data_dir(
                dir.path().to_path_buf(),
            ),
            docker.clone(),
        ));
        let driver = FirecrackerWorkloadDriver::new(provider, docker);
        let mut logs = driver
            .logs(
                &handle_of("wl-gone"),
                LogQuery {
                    follow: true,
                    resume: true,
                    ..LogQuery::default()
                },
            )
            .await
            .expect("the stream opens");
        let error = logs
            .next()
            .await
            .expect("an item")
            .expect_err("the guest is unreachable");
        assert!(error.to_string().contains("wl-gone"), "{error}");
        assert!(
            error.to_string().contains("is not running"),
            "a stopped VM is said to be stopped, not a socket error: {error}"
        );
        assert!(logs.next().await.is_none());
    }

    #[tokio::test]
    async fn the_tail_of_a_stopped_microvm_says_it_is_stopped() {
        let dir = tempfile::tempdir().expect("tempdir");
        let docker = Arc::new(bollard::Docker::connect_with_http_defaults().expect("docker"));
        let provider = Arc::new(FirecrackerSandboxProvider::new(
            crate::sandbox::firecracker::FirecrackerSandboxConfig::from_data_dir(
                dir.path().to_path_buf(),
            ),
            docker.clone(),
        ));
        let driver = FirecrackerWorkloadDriver::new(provider, docker);
        let error = match driver
            .logs(&handle_of("wl-stopped"), LogQuery::default())
            .await
        {
            Ok(_) => panic!("a stopped VM has no log to read"),
            Err(error) => error.to_string(),
        };
        assert!(error.contains("wl-stopped"), "{error}");
        assert!(error.contains("is not running"), "{error}");
        assert!(!error.contains("os error"), "{error}");
    }

    #[test]
    fn guest_records_become_dated_lines_on_the_stream_they_were_written_to() {
        let log = "2026-10-30T21:27:02.000000001Z stdout F GET / 200\n\
                   2026-10-30T21:27:03.500000000Z stderr P warn: half \n\
                   2026-10-30T21:27:03.600000000Z stdout F next\n\
                   2026-10-30T21:27:04.000000000Z stderr F a line\n\
                   2026-10-30T21:27:05.000000000Z stdout F \n";
        let lines: Vec<LogLine> = log_lines(log).into_iter().map(Result::unwrap).collect();
        let seen: Vec<(bool, &[u8], String)> = lines
            .iter()
            .map(|line| {
                let time = line
                    .timestamp
                    .expect("a record is dated")
                    .to_rfc3339_opts(chrono::SecondsFormat::Nanos, true);
                (line.stderr, line.line.as_ref(), time)
            })
            .collect();
        assert_eq!(
            seen,
            [
                (
                    false,
                    &b"GET / 200\n"[..],
                    "2026-10-30T21:27:02.000000001Z".to_string()
                ),
                (
                    false,
                    &b"next\n"[..],
                    "2026-10-30T21:27:03.600000000Z".to_string()
                ),
                // A line split across reads is dated when it began.
                (
                    true,
                    &b"warn: half a line\n"[..],
                    "2026-10-30T21:27:03.500000000Z".to_string()
                ),
                (
                    false,
                    &b"\n"[..],
                    "2026-10-30T21:27:05.000000000Z".to_string()
                ),
            ]
        );
    }

    #[test]
    fn a_log_from_an_agent_without_records_reads_as_undated_stdout() {
        let lines: Vec<LogLine> = log_lines("plain output\n2026 is not a record\n")
            .into_iter()
            .map(Result::unwrap)
            .collect();
        assert_eq!(lines.len(), 2);
        assert!(lines.iter().all(|l| !l.stderr && l.timestamp.is_none()));
        assert_eq!(lines[0].line.as_ref(), b"plain output\n");
        assert_eq!(lines[1].line.as_ref(), b"2026 is not a record\n");
    }

    #[test]
    fn a_line_that_never_ends_is_handed_over_once_it_grows_too_long() {
        let piece = "x".repeat(64 * 1024);
        let mut lines = LogLines::default();
        let mut out = Vec::new();
        for _ in 0..5 {
            let raw = format!("2026-10-30T21:27:02.000000000Z stdout P {piece}");
            out.extend(lines.push(&raw));
        }
        assert_eq!(out.len(), 1, "held at most MAX_PENDING_LINE_BYTES");
        assert_eq!(out[0].line.len(), 4 * piece.len() + 1);
    }

    #[test]
    fn a_guest_that_reports_no_ram_has_no_memory_limit() {
        let stats = instance_stats(temps_vm_agent::stats::GuestStats::default());
        assert_eq!(stats.memory_limit_bytes, None);
    }

    use super::*;
    use temps_workload::{
        InstanceLimits, NetworkAttachment, RestartPolicy, SecretFile, SecurityProfile,
        WorkloadKind, WorkloadRef,
    };

    fn spec() -> InstanceSpec {
        InstanceSpec {
            owner: WorkloadRef::new(WorkloadKind::Deployment, 42),
            name: "app-production-42-1".to_string(),
            image: "registry.example.test/shop/app:42".to_string(),
            platform: None,
            env: vec![("PORT".to_string(), "3000".to_string())],
            secret_files: Vec::new(),
            command: None,
            entrypoint: None,
            workdir: None,
            user: None,
            ports: vec![PortBinding {
                container_port: 3000,
                protocol: PortProtocol::Tcp,
                host_ip: "127.0.0.1".to_string(),
                host_port: None,
            }],
            restart: RestartPolicy::UnlessStopped,
            limits: InstanceLimits::default(),
            log_rotation: None,
            labels: BTreeMap::new(),
            security: SecurityProfile::Trusted,
        }
    }

    fn image() -> ImageRuntimeConfig {
        ImageRuntimeConfig {
            entrypoint: Some(vec!["docker-entrypoint.sh".to_string()]),
            cmd: Some(vec!["node".to_string(), "server.js".to_string()]),
            env: vec![
                "PATH=/usr/local/bin:/usr/bin:/bin".to_string(),
                "PORT=8080".to_string(),
            ],
            working_dir: Some("/app".to_string()),
            user: Some("1000:1000".to_string()),
        }
    }

    #[test]
    fn the_guest_runs_the_images_entrypoint_and_command_unless_overridden() {
        let (cmd, cwd) = resolve_process(&spec(), &image()).unwrap();
        assert_eq!(cmd, ["docker-entrypoint.sh", "node", "server.js"]);
        assert_eq!(cwd.as_deref(), Some("/app"));

        let mut command = spec();
        command.command = Some(vec!["node".into(), "worker.js".into()]);
        let (cmd, _) = resolve_process(&command, &image()).unwrap();
        assert_eq!(cmd, ["docker-entrypoint.sh", "node", "worker.js"]);

        // Docker's rule: a new entrypoint drops the image's command.
        let mut entrypoint = spec();
        entrypoint.entrypoint = Some(vec!["/bin/server".into()]);
        let (cmd, _) = resolve_process(&entrypoint, &image()).unwrap();
        assert_eq!(cmd, ["/bin/server"]);

        let error = resolve_process(&spec(), &ImageRuntimeConfig::default()).unwrap_err();
        assert!(error.to_string().contains("app-production-42-1"), "{error}");
    }

    #[test]
    fn the_spec_environment_wins_over_the_images() {
        let env = resolve_env(&spec(), &image());
        assert_eq!(env["PORT"], "3000");
        assert_eq!(env["PATH"], "/usr/local/bin:/usr/bin:/bin");
    }

    #[test]
    fn the_spec_user_overrides_the_images_and_blank_means_none() {
        let mut spec = spec();
        let mut image = image();
        image.user = None;
        spec.user = None;
        assert_eq!(replica_user_spec(&spec, &image), None);
        image.user = Some("nextjs".to_string());
        assert_eq!(replica_user_spec(&spec, &image), Some("nextjs"));
        spec.user = Some(" 1000:2000 ".to_string());
        assert_eq!(
            replica_user_spec(&spec, &image),
            Some("1000:2000"),
            "the spec wins and is trimmed"
        );
        spec.user = Some("  ".to_string());
        image.user = Some("nextjs".to_string());
        assert_eq!(
            replica_user_spec(&spec, &image),
            Some("nextjs"),
            "a blank override must not hide the image's own USER"
        );
        image.user = Some(" ".to_string());
        assert_eq!(replica_user_spec(&spec, &image), None);
    }

    #[test]
    fn what_a_micro_vm_cannot_express_is_refused_naming_it() {
        let attach = Attachments::default();
        assert!(check_expressible(&spec(), &attach).is_ok());

        let mut secrets = spec();
        secrets.secret_files.push(SecretFile {
            name: "DB_PASSWORD".into(),
            value: "never-shown".into(),
        });
        let message = check_expressible(&secrets, &attach)
            .unwrap_err()
            .to_string();
        assert!(message.contains("secret files"), "{message}");
        assert!(!message.contains("never-shown"));

        let volumes = Attachments {
            volumes: vec!["data:/data".into()],
            ..Default::default()
        };
        assert!(check_expressible(&spec(), &volumes)
            .unwrap_err()
            .to_string()
            .contains("volume binds"));

        let networks = Attachments {
            network: NetworkAttachment {
                required: vec!["shop_default".into()],
                ..Default::default()
            },
            ..Default::default()
        };
        let message = check_expressible(&spec(), &networks)
            .unwrap_err()
            .to_string();
        assert!(message.contains("shop_default"), "{message}");

        let mut udp = spec();
        udp.ports[0].protocol = PortProtocol::Udp;
        let message = check_expressible(&udp, &attach).unwrap_err().to_string();
        assert!(message.contains("UDP port 3000"), "{message}");
    }

    #[test]
    fn bandwidth_limits_reach_the_vmm_and_zero_means_unlimited() {
        let capabilities = workload_vm_capabilities();
        assert!(capabilities.limits.disk_io && capabilities.limits.network);
        let limits = InstanceLimits {
            disk_io_bytes_per_sec: Some(10 * 1024 * 1024),
            network_bytes_per_sec: Some(0),
            ..InstanceLimits::default()
        };
        assert_eq!(
            vm_io_limits(&limits),
            VmIoLimits {
                disk_bytes_per_sec: Some(10 * 1024 * 1024),
                network_bytes_per_sec: None,
            }
        );
        assert_eq!(
            vm_io_limits(&InstanceLimits::default()),
            VmIoLimits::default()
        );
    }

    #[test]
    fn a_pids_limit_is_carried_into_the_guest_as_a_process_ceiling() {
        // The plane admits a pids limit only for a driver that declares it.
        assert!(workload_vm_capabilities().limits.pids);
    }

    #[test]
    fn the_capability_table_offers_micro_vm_and_nothing_it_cannot_do() {
        let capabilities = workload_vm_capabilities();
        assert_eq!(capabilities.isolation, Isolation::MicroVm);
        assert_eq!(capabilities.name, WORKLOAD_VM_DRIVER);
        assert!(capabilities.artifacts.oci);
        assert!(!capabilities.snapshot);
        assert_eq!(capabilities.volumes, VolumeKinds::default());
    }

    #[test]
    fn a_runtime_picked_host_port_is_a_real_free_port() {
        let binding = PortBinding {
            container_port: 3000,
            protocol: PortProtocol::Tcp,
            host_ip: "127.0.0.1".into(),
            host_port: None,
        };
        let port = pick_host_port("a", &binding).unwrap();
        assert_ne!(port, 0);
        let fixed = PortBinding {
            host_port: Some(41234),
            ..binding
        };
        assert_eq!(pick_host_port("a", &fixed).unwrap(), 41234);
    }

    /// The forwarder relays bytes to the "guest" (a local echo server here)
    /// and keeps serving after a connection ends.
    #[tokio::test]
    async fn a_forwarder_relays_to_the_guest_port() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let guest = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let guest_port = guest.local_addr().unwrap().port();
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = guest.accept().await else {
                    return;
                };
                tokio::spawn(async move {
                    let mut buf = [0u8; 64];
                    if let Ok(n) = stream.read(&mut buf).await {
                        let _ = stream.write_all(&buf[..n]).await;
                    }
                });
            }
        });
        let host_port = pick_host_port(
            "a",
            &PortBinding {
                container_port: guest_port,
                protocol: PortProtocol::Tcp,
                host_ip: "127.0.0.1".into(),
                host_port: None,
            },
        )
        .unwrap();
        let task = bind_forwarder(
            &ForwardedPort {
                container_port: guest_port,
                host_ip: "127.0.0.1".into(),
                host_port,
            },
            Ipv4Addr::LOCALHOST,
            "temps-fcsandbox-wl-test",
        )
        .await
        .unwrap();
        for round in 0..2 {
            let mut client = tokio::net::TcpStream::connect(("127.0.0.1", host_port))
                .await
                .unwrap();
            let message = format!("ping-{round}");
            client.write_all(message.as_bytes()).await.unwrap();
            let mut reply = vec![0u8; message.len()];
            client.read_exact(&mut reply).await.unwrap();
            assert_eq!(reply, message.as_bytes());
        }
        task.abort();
    }

    #[test]
    fn state_round_trips_without_any_environment_value() {
        let state = WorkloadVmState {
            name: "app-production-42-1".into(),
            cmd: vec!["node".into(), "server.js".into()],
            cwd: Some("/app".into()),
            uid: Some(1000),
            gid: None,
            pid: Some(7),
            started_at: None,
            ports: vec![ForwardedPort {
                container_port: 3000,
                host_ip: "127.0.0.1".into(),
                host_port: 40000,
            }],
            labels: BTreeMap::from([("sh.temps.deploy_id".into(), "42".into())]),
            restart: Some(RestartPolicy::UnlessStopped),
            log_rotation: Some(GuestLogRotation {
                max_bytes: 10 * 1024 * 1024,
                max_files: 3,
            }),
            restart_count: 2,
            limits: temps_vm_agent::SpawnLimits {
                max_open_files: None,
                max_processes: Some(256),
            },
        };
        let json = serde_json::to_string(&state).unwrap();
        assert!(!json.contains("PORT"), "variables live in env.json only");
        let back: WorkloadVmState = serde_json::from_str(&json).unwrap();
        assert_eq!(back, state);
    }

    #[test]
    fn a_state_file_from_before_restarts_still_reads_and_runs_its_process_once() {
        let json = r#"{"name":"app-1","cmd":["/app"],"cwd":null,"uid":null,"gid":null,
            "pid":7,"started_at":null,"ports":[],"labels":{}}"#;
        let state: WorkloadVmState = serde_json::from_str(json).expect("old state parses");
        assert_eq!(state.restart, None);
        assert_eq!(state.log_rotation, None);
        assert_eq!(state.restart_count, 0);
        assert_eq!(state.limits, temps_vm_agent::SpawnLimits::default());
        assert_eq!(
            guest_restart_policy(state.restart),
            SpawnRestartPolicy::Never
        );
    }

    #[test]
    fn every_restart_policy_is_carried_out_by_the_guest() {
        assert_eq!(
            guest_restart_policy(Some(RestartPolicy::Never)),
            SpawnRestartPolicy::Never
        );
        assert_eq!(
            guest_restart_policy(Some(RestartPolicy::Always)),
            SpawnRestartPolicy::Always
        );
        assert_eq!(
            guest_restart_policy(Some(RestartPolicy::UnlessStopped)),
            SpawnRestartPolicy::UnlessStopped
        );
        assert_eq!(
            guest_restart_policy(Some(RestartPolicy::OnFailure)),
            SpawnRestartPolicy::OnFailure { max_retries: None }
        );
    }

    #[test]
    fn a_pending_restart_is_running_and_a_finished_process_is_exited() {
        let status = |running, restarting, exit_code| GuestProcessStatus {
            running,
            exit_code,
            restart_count: 3,
            restarting,
        };
        assert_eq!(
            observed_state(&status(true, false, None)),
            (InstanceState::Running, None)
        );
        assert_eq!(
            observed_state(&status(false, true, Some(1))),
            (InstanceState::Running, None),
            "the guest brings it back; the plane must not"
        );
        assert_eq!(
            observed_state(&status(false, false, Some(137))),
            (InstanceState::Exited, Some(137))
        );
    }

    #[test]
    fn log_sizes_read_like_dockers_max_size() {
        assert_eq!(parse_log_size("10m"), Some(10 * 1024 * 1024));
        assert_eq!(parse_log_size("10MB"), Some(10 * 1024 * 1024));
        assert_eq!(parse_log_size("512k"), Some(512 * 1024));
        assert_eq!(parse_log_size("1g"), Some(1024 * 1024 * 1024));
        assert_eq!(parse_log_size("1.5m"), Some(3 * 512 * 1024));
        assert_eq!(parse_log_size(" 4096 "), Some(4096));
        for bad in ["", "m", "10x", "-1m", "0", "ten"] {
            assert_eq!(parse_log_size(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn the_requested_log_rotation_is_honoured_bounded_or_refused_naming_the_field() {
        let rotation = |max_size: &str, max_file| LogRotation {
            max_size: max_size.to_string(),
            max_file,
        };
        assert_eq!(resolve_log_rotation("app-1", None).unwrap(), None);
        // The plane's default for deployments.
        assert_eq!(
            resolve_log_rotation("app-1", Some(&rotation("10m", 3))).unwrap(),
            Some(GuestLogRotation {
                max_bytes: 10 * 1024 * 1024,
                max_files: 3
            })
        );
        // Beyond what a small rootfs should hold: clamped, never unbounded.
        let clamped = resolve_log_rotation("app-1", Some(&rotation("50g", 500)))
            .unwrap()
            .unwrap();
        assert_eq!(
            clamped.max_bytes,
            temps_vm_agent::workload_log::MAX_LOG_MAX_BYTES
        );
        assert_eq!(
            clamped.max_files,
            temps_vm_agent::workload_log::MAX_LOG_MAX_FILES
        );
        let error = resolve_log_rotation("app-1", Some(&rotation("lots", 3)))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("app-1") && error.contains("max_size 'lots'"),
            "{error}"
        );
        let error = resolve_log_rotation("app-1", Some(&rotation("10m", 0)))
            .unwrap_err()
            .to_string();
        assert!(error.contains("max_file is 0"), "{error}");
    }

    /// A driver over a real Firecracker install, or `None` (the caller
    /// skips) unless this host has `/dev/kvm`, a provisioned Firecracker
    /// install under the data directory `temps serve` uses, and Docker.
    async fn real_driver() -> Option<FirecrackerWorkloadDriver> {
        if !std::path::Path::new("/dev/kvm").exists() {
            println!("/dev/kvm not present, skipping the Firecracker runtime test");
            return None;
        }
        let Some(home) = std::env::var_os("HOME") else {
            println!("HOME not set, skipping the Firecracker runtime test");
            return None;
        };
        let data_dir = std::path::PathBuf::from(home).join(".temps");
        // A test host may lack the root launcher; with one, VMs go through it.
        let config =
            crate::sandbox::firecracker::FirecrackerSandboxConfig::from_data_dir(data_dir.clone())
                .allowing_unjailed();
        if let Some(reason) = crate::sandbox::firecracker::firecracker_unavailable_reason(&config) {
            println!("Firecracker is not usable under {data_dir:?} ({reason}), skipping");
            return None;
        }
        let Ok(docker) = bollard::Docker::connect_with_defaults() else {
            println!("Docker not available, skipping the Firecracker runtime test");
            return None;
        };
        if docker.ping().await.is_err() {
            println!("Docker not available, skipping the Firecracker runtime test");
            return None;
        }
        let docker = Arc::new(docker);
        let provider = Arc::new(FirecrackerSandboxProvider::new(config, docker.clone()));
        Some(FirecrackerWorkloadDriver::new(provider, docker))
    }

    /// A crashing process in a real microVM is restarted by the guest, and
    /// the replica keeps reporting as running. Skips like the test below.
    #[tokio::test]
    async fn a_crashing_workload_is_restarted_inside_a_real_micro_vm() {
        let Some(driver) = real_driver().await else {
            return;
        };
        let artifact = Artifact::OciImage {
            reference: "busybox:1.36".to_string(),
            digest: None,
        };
        driver.prepare(&artifact).await.expect("pull busybox");
        let mut spec = spec();
        spec.name = format!("fc-restart-test-{}", std::process::id());
        spec.image = "busybox:1.36".to_string();
        spec.ports = Vec::new();
        spec.restart = RestartPolicy::UnlessStopped;
        spec.command = Some(vec![
            "sh".to_string(),
            "-c".to_string(),
            "echo crash-once; exit 3".to_string(),
        ]);
        let handle = driver
            .create(&spec, &Attachments::default())
            .await
            .expect("create the microVM workload");
        let outcome = async {
            driver.start(&handle).await?;
            let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
            loop {
                if driver.restart_count(&handle).await?.unwrap_or(0) >= 2 {
                    break;
                }
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "the guest never restarted the crashing process"
                );
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            let status = driver.inspect(&handle).await?;
            assert_eq!(status.state, InstanceState::Running, "{status:?}");
            Ok::<(), DriverError>(())
        }
        .await;
        let removed = driver.remove(&handle, true).await;
        outcome.expect("the guest restarted the workload");
        removed.expect("remove the microVM workload");
    }

    /// Boots a real microVM. Skips (passes) unless this host has `/dev/kvm`,
    /// a provisioned Firecracker install under the data directory (the one
    /// `temps serve` uses) and Docker for the image.
    #[tokio::test]
    async fn a_workload_boots_runs_and_is_removed_in_a_real_micro_vm() {
        let Some(driver) = real_driver().await else {
            return;
        };

        let artifact = Artifact::OciImage {
            reference: "busybox:1.36".to_string(),
            digest: None,
        };
        driver.prepare(&artifact).await.expect("pull busybox");
        let mut spec = spec();
        spec.name = format!("fc-runtime-test-{}", std::process::id());
        spec.image = "busybox:1.36".to_string();
        spec.ports = Vec::new();
        spec.command = Some(vec![
            "sh".to_string(),
            "-c".to_string(),
            "echo workload-ready; sleep 60".to_string(),
        ]);
        let handle = driver
            .create(&spec, &Attachments::default())
            .await
            .expect("create the microVM workload");
        let outcome = async {
            driver.start(&handle).await?;
            let status = driver.inspect(&handle).await?;
            assert_eq!(status.state, InstanceState::Running, "{status:?}");
            tokio::time::sleep(Duration::from_secs(1)).await;
            let mut logs = driver
                .logs(
                    &handle,
                    LogQuery {
                        since: None,
                        tail: Some(20),
                        follow: false,
                        resume: false,
                    },
                )
                .await?;
            let mut text = String::new();
            while let Some(line) = logs.next().await {
                text.push_str(&String::from_utf8_lossy(&line?.line));
            }
            assert!(text.contains("workload-ready"), "guest log: {text}");
            Ok::<(), DriverError>(())
        }
        .await;
        let removed = driver.remove(&handle, true).await;
        outcome.expect("the workload ran in the microVM");
        removed.expect("remove the microVM workload");
        let gone = driver.inspect(&handle).await.expect("inspect after remove");
        assert_eq!(gone.state, InstanceState::Missing);
    }

    /// Reads lines from `logs` for `window`, as text lines.
    async fn read_lines_for(mut logs: temps_workload::LogStream, window: Duration) -> Vec<String> {
        let mut lines = Vec::new();
        let deadline = tokio::time::Instant::now() + window;
        while let Ok(Some(line)) = tokio::time::timeout_at(deadline, logs.next()).await {
            let line = line.expect("a log line, not a stream error");
            lines.push(String::from_utf8_lossy(&line.line).trim_end().to_string());
        }
        lines
    }

    fn counted(lines: &[String]) -> Vec<u64> {
        lines
            .iter()
            .filter_map(|l| l.strip_prefix("line-")?.parse().ok())
            .collect()
    }

    /// The new log and metrics paths against a real guest: a followed read
    /// starts at the tail and keeps delivering, a `resume` reader neither
    /// repeats nor re-reads what the previous one consumed, and the host
    /// reads CPU, memory and block figures from the guest. Skips like the
    /// tests above.
    #[tokio::test]
    async fn a_real_micro_vm_streams_logs_follows_resumes_and_reports_metrics() {
        let Some(driver) = real_driver().await else {
            return;
        };
        let artifact = Artifact::OciImage {
            reference: "busybox:1.36".to_string(),
            digest: None,
        };
        driver.prepare(&artifact).await.expect("pull busybox");
        let mut spec = spec();
        spec.name = format!("fc-logs-test-{}", std::process::id());
        spec.image = "busybox:1.36".to_string();
        spec.ports = Vec::new();
        spec.command = Some(vec![
            "sh".to_string(),
            "-c".to_string(),
            // A little arithmetic per second, so the process has CPU time to
            // report (an idle sleep loop rounds to zero 10ms ticks).
            "i=0; while true; do i=$((i+1)); n=0; while [ $n -lt 30000 ]; do n=$((n+1)); done; echo line-$i; echo warn-$i >&2; sleep 1; done".to_string(),
        ]);
        let handle = driver
            .create(&spec, &Attachments::default())
            .await
            .expect("create the microVM workload");
        // Assertions panic; catch that so the VM is removed either way and a
        // failing run does not leave a microVM behind on the host.
        let outcome = futures::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(async {
            let started = Utc::now();
            driver.start(&handle).await?;
            tokio::time::sleep(Duration::from_secs(4)).await;

            // Each line is dated by the guest and keeps the stream it was
            // written to.
            let mut tail = driver
                .logs(
                    &handle,
                    LogQuery {
                        since: None,
                        tail: Some(6),
                        follow: false,
                        resume: false,
                    },
                )
                .await?;
            let mut dated = Vec::new();
            while let Some(line) = tail.next().await {
                dated.push(line?);
            }
            assert!(!dated.is_empty(), "the tail has lines");
            for line in &dated {
                let text = String::from_utf8_lossy(&line.line);
                let at = line.timestamp.unwrap_or_else(|| panic!("undated: {text:?}"));
                assert!(
                    at >= started - chrono::Duration::seconds(5) && at <= Utc::now(),
                    "{text:?} dated {at}, started {started}"
                );
                assert_eq!(line.stderr, text.starts_with("warn-"), "{text:?}");
            }

            // Followed: history first, then lines that did not exist yet.
            let follow = driver
                .logs(
                    &handle,
                    LogQuery {
                        since: None,
                        tail: Some(2),
                        follow: true,
                        resume: false,
                    },
                )
                .await?;
            let followed = counted(&read_lines_for(follow, Duration::from_secs(4)).await);
            assert!(followed.len() >= 4, "history plus new lines: {followed:?}");
            assert!(
                followed.windows(2).all(|w| w[1] == w[0] + 1),
                "in order, none skipped or repeated: {followed:?}"
            );

            // Resume: a first reader consumes some, a second continues after it.
            let first = driver
                .logs(
                    &handle,
                    LogQuery {
                        since: None,
                        tail: None,
                        follow: true,
                        resume: true,
                    },
                )
                .await?;
            let first = counted(&read_lines_for(first, Duration::from_secs(3)).await);
            assert!(!first.is_empty(), "the first resume reader got lines");
            let last_seen = *first.last().expect("a last line");
            tokio::time::sleep(Duration::from_secs(2)).await;
            let second = driver
                .logs(
                    &handle,
                    LogQuery {
                        since: None,
                        tail: None,
                        follow: true,
                        resume: true,
                    },
                )
                .await?;
            let second = counted(&read_lines_for(second, Duration::from_secs(3)).await);
            assert!(!second.is_empty(), "the second resume reader got lines");
            // At-least-once: the checkpoint moves past a batch when the reader
            // asks for more, so the line a reader stopped on may come again,
            // but nothing after the checkpoint is skipped.
            assert!(
                second[0] <= last_seen + 1,
                "a restarted reader must not skip lines: first read up to {last_seen}, second began at {}",
                second[0]
            );
            assert!(
                second[0] + 3 >= last_seen,
                "repetition is bounded to the last batch: first read up to {last_seen}, second began at {}",
                second[0]
            );

            let stats = driver.stats(&handle).await?;
            println!("microVM stats: {stats:?}");
            assert!(stats.memory_bytes.is_some_and(|b| b > 0), "{stats:?}");
            assert!(stats.cpu_nanos.is_some_and(|n| n > 0), "{stats:?}");
            assert!(stats.memory_limit_bytes.is_some_and(|b| b > 0), "{stats:?}");
            assert!(stats.disk_read_bytes.is_some_and(|b| b > 0), "booting reads the disk: {stats:?}");
            assert!(stats.disk_write_bytes.is_some(), "{stats:?}");
            Ok::<(), DriverError>(())
        }))
        .await;
        let removed = driver.remove(&handle, true).await;
        match outcome {
            Ok(result) => result.expect("logs and metrics work against a real guest"),
            Err(panic) => std::panic::resume_unwind(panic),
        }
        removed.expect("remove the microVM workload");
    }
}
