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
//! What a microVM cannot express here is refused at create with the field
//! named, never dropped: secret files, volume binds, required Docker
//! networks, UDP ports and non-numeric users.
//!
//! Memory: one small JSON state file per VM on disk, and in memory one
//! accept task per published port plus one copy task per open connection
//! (bounded by [`MAX_CONNECTIONS_PER_PORT`]).

use std::collections::{BTreeMap, HashMap};
use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use chrono::{DateTime, Utc};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use temps_workload::{
    Artifact, ArtifactKinds, AttachmentKind, Attachments, DriverCapabilities, DriverError,
    ExecSpec, ExecStream, InstanceHandle, InstanceSpec, InstanceState, InstanceStats,
    InstanceStatus, Isolation, LimitSupport, ListedInstance, LogLine, LogQuery, LogStream,
    PortBinding, PortProtocol, PreparedArtifact, RuntimeDriver, VolumeKinds,
};

use super::firecracker::FirecrackerSandboxProvider;
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
    /// The guest pid of the running process, once started.
    pid: Option<u32>,
    started_at: Option<DateTime<Utc>>,
    ports: Vec<ForwardedPort>,
    labels: BTreeMap<String, String>,
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

/// A numeric `uid[:gid]` (or `root`). A user name would need the guest's
/// `/etc/passwd`, which the agent does not read: refused with the reason.
pub fn resolve_user(
    instance: &str,
    user: Option<&str>,
) -> Result<(Option<u32>, Option<u32>), DriverError> {
    let Some(user) = user.map(str::trim).filter(|user| !user.is_empty()) else {
        return Ok((None, None));
    };
    if user == "root" {
        return Ok((Some(0), Some(0)));
    }
    let (uid, gid) = match user.split_once(':') {
        Some((uid, gid)) => (uid, Some(gid)),
        None => (user, None),
    };
    let parse = |part: &str| part.parse::<u32>().ok();
    match (parse(uid), gid.map(parse)) {
        (Some(uid), None) => Ok((Some(uid), None)),
        (Some(uid), Some(Some(gid))) => Ok((Some(uid), Some(gid))),
        _ => Err(DriverError::Runtime {
            driver: WORKLOAD_VM_DRIVER.to_string(),
            operation: "resolve the user of",
            instance: instance.to_string(),
            reason: format!(
                "user '{user}' is not numeric; a microVM replica needs `uid` or `uid:gid` \
                 (set USER to a number in the image, or keep this environment on process \
                 isolation)"
            ),
        }),
    }
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

    async fn process_running(
        &self,
        vm_id: &str,
        pid: u32,
    ) -> Result<(bool, Option<i32>), DriverError> {
        self.provider
            .process_status(vm_id, pid)
            .await
            .map_err(|e| backend_error("inspect", vm_id, e))
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
            pids: false,
            disk: true,
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
        let image = self.image_config(&spec.name, &spec.image).await?;
        let (cmd, cwd) = resolve_process(spec, &image)?;
        let (uid, gid) = resolve_user(&spec.name, spec.user.as_deref().or(image.user.as_deref()))?;
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
            network_mode: Some("full".to_string()),
            // Persisted by the backend in the 0700 VM dir as a 0600 file,
            // exactly as a sandbox's variables, so a restart can respawn.
            env_vars: resolve_env(spec, &image),
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
        let running = match state.pid {
            Some(pid) => self.process_running(vm_id, pid).await?.0,
            None => false,
        };
        if !running {
            let env = self.provider.workload_env(vm_id);
            let pid = self
                .provider
                .spawn_process(
                    vm_id,
                    state.cmd.clone(),
                    env,
                    state.cwd.clone(),
                    state.uid,
                    state.gid,
                )
                .await
                .map_err(|e| backend_error("start", vm_id, e))?;
            state.pid = Some(pid);
            state.started_at = Some(Utc::now());
            self.write_state(vm_id, &state)?;
            tracing::info!(vm_id = %vm_id, pid, "microVM replica process started");
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
        let Some(state) = self.read_state(vm_id) else {
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
            (true, Some(pid)) => match self.process_running(vm_id, pid).await? {
                (true, _) => {
                    if let Err(error) = self.ensure_forwarders(vm_id, &state).await {
                        tracing::warn!(vm_id = %vm_id, %error, "could not re-open the microVM's forwarders");
                    }
                    (InstanceState::Running, None)
                }
                (false, code) => (InstanceState::Exited, code.map(i64::from)),
            },
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
        if query.follow {
            return Err(unsupported(vm_id, "following logs of a microVM replica"));
        }
        let tail = query.tail.unwrap_or(DEFAULT_LOG_TAIL).max(1);
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
            .map_err(|e| backend_error("read the logs of", vm_id, e))?;
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
        let lines: Vec<Result<LogLine, DriverError>> = result
            .stdout
            .lines()
            .map(|line| {
                Ok(LogLine {
                    stderr: false,
                    line: Bytes::from(format!("{line}\n")),
                })
            })
            .collect();
        Ok(futures::stream::iter(lines).boxed())
    }

    async fn stats(&self, _handle: &InstanceHandle) -> Result<InstanceStats, DriverError> {
        // The VMM's own usage is not the workload's: report unknown rather
        // than a misleading number.
        Ok(InstanceStats::default())
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
            }));
        }
        for line in result.stderr.lines() {
            lines.push(Ok(LogLine {
                stderr: true,
                line: Bytes::from(format!("{line}\n")),
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
    fn only_numeric_users_are_accepted() {
        assert_eq!(resolve_user("a", None).unwrap(), (None, None));
        assert_eq!(resolve_user("a", Some("")).unwrap(), (None, None));
        assert_eq!(resolve_user("a", Some("root")).unwrap(), (Some(0), Some(0)));
        assert_eq!(resolve_user("a", Some("1000")).unwrap(), (Some(1000), None));
        assert_eq!(
            resolve_user("a", Some("1000:2000")).unwrap(),
            (Some(1000), Some(2000))
        );
        let error = resolve_user("app-1", Some("node")).unwrap_err().to_string();
        assert!(
            error.contains("app-1") && error.contains("'node'"),
            "{error}"
        );
        assert!(resolve_user("a", Some("1000:staff")).is_err());
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
        };
        let json = serde_json::to_string(&state).unwrap();
        assert!(!json.contains("PORT"), "variables live in env.json only");
        let back: WorkloadVmState = serde_json::from_str(&json).unwrap();
        assert_eq!(back, state);
    }

    /// Boots a real microVM. Skips (passes) unless this host has `/dev/kvm`,
    /// a provisioned Firecracker install under the data directory (the one
    /// `temps serve` uses) and Docker for the image.
    #[tokio::test]
    async fn a_workload_boots_runs_and_is_removed_in_a_real_micro_vm() {
        if !std::path::Path::new("/dev/kvm").exists() {
            println!("/dev/kvm not present, skipping the Firecracker runtime test");
            return;
        }
        let Some(home) = std::env::var_os("HOME") else {
            println!("HOME not set, skipping the Firecracker runtime test");
            return;
        };
        let data_dir = std::path::PathBuf::from(home).join(".temps");
        if !crate::sandbox::firecracker::is_firecracker_available(&data_dir).await {
            println!("Firecracker is not provisioned under {data_dir:?}, skipping");
            return;
        }
        let Ok(docker) = bollard::Docker::connect_with_defaults() else {
            println!("Docker not available, skipping the Firecracker runtime test");
            return;
        };
        if docker.ping().await.is_err() {
            println!("Docker not available, skipping the Firecracker runtime test");
            return;
        }
        let docker = Arc::new(docker);
        let provider = Arc::new(FirecrackerSandboxProvider::new(
            crate::sandbox::firecracker::FirecrackerSandboxConfig::from_data_dir(data_dir),
            docker.clone(),
        ));
        let driver = FirecrackerWorkloadDriver::new(provider, docker);

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
}
