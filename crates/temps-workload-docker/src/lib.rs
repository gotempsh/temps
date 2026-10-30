// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The Docker [`RuntimeDriver`] (ADR-055 §6).
//!
//! [`container_create_body`] is the one function that turns an
//! [`InstanceSpec`] into Docker's container-create request. Every stack that
//! creates a workload container moves onto it (deployments in phase 1,
//! databases in phase 4, sandboxes in phase 6), which is how the hardening,
//! limits and labels stop drifting between them. The golden captures in
//! `temps-deployer/tests/golden_container_create.rs` pin it to the request
//! `DockerRuntime` sent before the extraction, byte for byte.
//!
//! Host-local decisions stay with the caller and arrive as binds: where
//! secret files are written and who owns them, and whether this host grants
//! the Docker socket (ADR-045).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use bollard::models::{
    ContainerCreateBody, HostConfig, HostConfigLogConfig, NetworkConnectRequest,
    RestartPolicyNameEnum,
};
use bollard::query_parameters::{
    CreateContainerOptionsBuilder, CreateImageOptionsBuilder, InspectContainerOptions,
    ListContainersOptions, LogsOptions, RemoveContainerOptions, StartContainerOptions,
    StatsOptions, StopContainerOptions, WaitContainerOptions,
};
use bollard::Docker;
use futures::{StreamExt, TryStreamExt};
pub mod data;
pub mod secrets;
pub mod shape;
pub mod volume;

pub use data::{data_container_create_body, data_host_config, DataExtras, DATA_DEFAULT_PIDS_LIMIT};
pub use shape::{diff as shape_diff, ContainerShape, ShapeDifference, ShapeField};
pub use volume::DockerVolumeProvider;

use temps_workload::{
    Artifact, ArtifactKinds, AttachmentKind, Attachments, DriverCapabilities, DriverError,
    InstanceHandle, InstanceSpec, InstanceState, InstanceStats, InstanceStatus, Isolation,
    LimitSupport, ListedInstance, LogLine, LogQuery, LogRotation, LogStream, PortProtocol,
    PreparedArtifact, RestartPolicy, RuntimeDriver, SecurityProfile, VolumeKinds,
};

/// The driver's name in handles, capabilities and errors.
pub const DRIVER: &str = "docker";

/// The PID cap every workload container gets unless its spec sets one.
pub const DEFAULT_PIDS_LIMIT: i64 = 512;

/// The hardened host config every workload container starts from.
///
/// Capability drops, `no-new-privileges`, the PID limit and the init
/// process are not optional. Binds are collected rather than assigned, so
/// adding one can never drop another (the secrets mount in particular).
pub fn hardened_host_config(binds: Vec<String>) -> HostConfig {
    HostConfig {
        // Drop every Linux capability.
        cap_drop: Some(vec!["ALL".to_string()]),
        // No privilege escalation through setuid/setgid binaries.
        security_opt: Some(vec!["no-new-privileges:true".to_string()]),
        // No fork bombs.
        pids_limit: Some(DEFAULT_PIDS_LIMIT),
        // An init process for signal handling and zombie reaping.
        init: Some(true),
        binds: (!binds.is_empty()).then_some(binds),
        ..Default::default()
    }
}

pub fn restart_policy_name(policy: RestartPolicy) -> RestartPolicyNameEnum {
    match policy {
        RestartPolicy::Never => RestartPolicyNameEnum::NO,
        RestartPolicy::Always => RestartPolicyNameEnum::ALWAYS,
        RestartPolicy::OnFailure => RestartPolicyNameEnum::ON_FAILURE,
        RestartPolicy::UnlessStopped => RestartPolicyNameEnum::UNLESS_STOPPED,
    }
}

pub fn log_config(rotation: &LogRotation) -> HostConfigLogConfig {
    HostConfigLogConfig {
        typ: Some("json-file".to_string()),
        config: Some(HashMap::from([
            ("max-size".to_string(), rotation.max_size.clone()),
            ("max-file".to_string(), rotation.max_file.to_string()),
        ])),
    }
}

fn protocol(protocol: PortProtocol) -> &'static str {
    match protocol {
        PortProtocol::Tcp => "tcp",
        PortProtocol::Udp => "udp",
    }
}

/// The container-create request for `spec`.
///
/// `binds` are the caller's host-local binds (the secrets directory, the
/// ADR-045 socket), in order; the attachment's volume binds follow them.
/// Secret values are never part of the request: `spec.secret_files` are
/// delivered through a bind the caller prepared.
pub fn container_create_body(
    spec: &InstanceSpec,
    attach: &Attachments,
    binds: Vec<String>,
) -> ContainerCreateBody {
    let mut port_bindings = HashMap::new();
    let mut exposed_ports = Vec::with_capacity(spec.ports.len());
    for port in &spec.ports {
        let key = format!("{}/{}", port.container_port, protocol(port.protocol));
        port_bindings.insert(
            key.clone(),
            Some(vec![bollard::models::PortBinding {
                host_ip: Some(port.host_ip.clone()),
                host_port: port.host_port.map(|p| p.to_string()),
            }]),
        );
        exposed_ports.push(key);
    }

    let binds = binds
        .into_iter()
        .chain(attach.volumes.iter().cloned())
        .collect();
    // `Data` (databases) is the control-plane database host config; every
    // other profile starts from the hardened one.
    let mut hardened = match spec.security {
        SecurityProfile::Data => data_host_config(binds),
        _ => hardened_host_config(binds),
    };
    if let Some(pids) = spec.limits.pids {
        hardened.pids_limit = Some(pids);
    }
    let host_config = HostConfig {
        port_bindings: Some(port_bindings),
        network_mode: attach.network.primary.clone(),
        dns: attach.network.dns.clone(),
        restart_policy: Some(bollard::models::RestartPolicy {
            name: Some(restart_policy_name(spec.restart)),
            ..Default::default()
        }),
        memory: spec.limits.memory_bytes,
        memory_swap: spec.limits.memory_swap_bytes,
        nano_cpus: spec.limits.nano_cpus,
        log_config: spec.log_rotation.as_ref().map(log_config),
        ..hardened
    };

    ContainerCreateBody {
        image: Some(spec.image.clone()),
        env: Some(spec.env.iter().map(|(k, v)| format!("{k}={v}")).collect()),
        exposed_ports: Some(exposed_ports),
        host_config: Some(host_config),
        cmd: spec.command.clone(),
        entrypoint: docker_entrypoint_override(spec.entrypoint.as_deref()),
        working_dir: spec.workdir.clone(),
        user: spec.user.clone(),
        labels: (!spec.labels.is_empty()).then(|| spec.labels.clone().into_iter().collect()),
        ..Default::default()
    }
}

pub fn capabilities() -> DriverCapabilities {
    DriverCapabilities {
        name: DRIVER.to_string(),
        artifacts: ArtifactKinds {
            oci: true,
            wasm: false,
            rootfs: false,
        },
        isolation: Isolation::Process,
        network_attachment: AttachmentKind::DockerNetwork,
        volumes: VolumeKinds {
            named: true,
            block_image: false,
            preopened_dir: false,
        },
        exec: false,
        snapshot: false,
        resize_disk: false,
        limits: LimitSupport {
            cpu: true,
            memory: true,
            pids: true,
            disk: false,
            disk_io: false,
            network: false,
        },
    }
}

fn runtime<'a>(
    operation: &'static str,
    instance: &'a str,
) -> impl FnOnce(bollard::errors::Error) -> DriverError + 'a {
    move |e| DriverError::Runtime {
        driver: DRIVER.to_string(),
        operation,
        instance: instance.to_string(),
        reason: e.to_string(),
    }
}

fn is_not_found(error: &bollard::errors::Error) -> bool {
    matches!(
        error,
        bollard::errors::Error::DockerResponseServerError {
            status_code: 404,
            ..
        }
    )
}

pub struct DockerDriver {
    docker: Docker,
    capabilities: DriverCapabilities,
    /// Where per-instance secret directories live (`<root>/<instance>`).
    /// `None`: specs with secret files are refused.
    secrets_root: Option<PathBuf>,
    /// Whether the daemon places containers with systemd on cgroup v2, so a
    /// workload slice can be their cgroup parent. Asked once, on the first
    /// create.
    slices: Arc<tokio::sync::OnceCell<bool>>,
}

impl DockerDriver {
    pub fn new(docker: Docker) -> Self {
        Self {
            docker,
            capabilities: capabilities(),
            secrets_root: None,
            slices: Arc::new(tokio::sync::OnceCell::new()),
        }
    }

    /// Deliver secret files at `/run/secrets` from `<root>/<instance>`
    /// (mode 0700, files 0400, owned by the image's numeric `USER`), the way
    /// the legacy deployer does. The root must survive a reboot: the
    /// directory is bind-mounted for the container's whole life.
    pub fn with_secrets_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.secrets_root = Some(root.into());
        self
    }

    /// Write `spec`'s secret files and return the bind that mounts them,
    /// or `None` when it has none.
    async fn write_secret_files(
        &self,
        spec: &InstanceSpec,
    ) -> Result<Option<(PathBuf, String)>, DriverError> {
        if spec.secret_files.is_empty() {
            return Ok(None);
        }
        let secrets_error = |reason: String| DriverError::Secrets {
            driver: DRIVER.to_string(),
            instance: spec.name.clone(),
            reason,
        };
        let Some(root) = &self.secrets_root else {
            return Err(secrets_error(
                "secret files need a host directory and this driver has no secrets root; \
                 create through a driver built with a secrets root"
                    .to_string(),
            ));
        };
        secrets::validate_secret_dir_name(&spec.name)
            .map_err(|e| secrets_error(format!("derive host dir: {e}")))?;
        let dir = root.join(&spec.name);
        let files: HashMap<String, String> = spec
            .secret_files
            .iter()
            .map(|file| (file.name.clone(), file.value.clone()))
            .collect();
        let owner = secrets::image_owner(&self.docker, &spec.image).await;
        secrets::write_secrets_to_host_dir(&dir, &files, Some(owner))
            .map_err(|e| secrets_error(format!("write host dir {}: {e}", dir.display())))?;
        let bind = secrets::secrets_bind(&dir);
        Ok(Some((dir, bind)))
    }

    /// The secrets directory of instance `name`, when this driver has one.
    fn secrets_dir_of(&self, name: &str) -> Option<PathBuf> {
        let root = self.secrets_root.as_ref()?;
        secrets::validate_secret_dir_name(name).ok()?;
        Some(root.join(name))
    }

    /// Creates the container with the caller's host-local binds (see
    /// [`container_create_body`]). The create of [`RuntimeDriver`] is this
    /// with no binds, and refuses specs carrying secret files, which need a
    /// host directory only the caller can own.
    pub async fn create_with_binds(
        &self,
        spec: &InstanceSpec,
        attach: &Attachments,
        binds: Vec<String>,
    ) -> Result<InstanceHandle, DriverError> {
        let body = container_create_body(spec, attach, binds);
        self.create_from_body(spec, body).await
    }

    /// Creates a database container: the `Data` request of
    /// [`data_container_create_body`] (healthcheck from the probes, the
    /// database's extra host settings). Both the control plane and the worker
    /// agent create managed databases through this (ADR-055 Phase 4).
    pub async fn create_data_with_binds(
        &self,
        spec: &InstanceSpec,
        probes: &temps_workload::Probes,
        extras: &DataExtras,
        attach: &Attachments,
        binds: Vec<String>,
    ) -> Result<InstanceHandle, DriverError> {
        let body = data_container_create_body(spec, probes, extras, attach, binds);
        self.create_from_body(spec, body).await
    }

    /// Whether the daemon uses the systemd cgroup driver on cgroup v2: then
    /// a slice name as cgroup parent nests the container's scope under it,
    /// and systemd creates the slice when the first container starts.
    async fn supports_slices(&self) -> bool {
        *self
            .slices
            .get_or_init(|| async {
                match self.docker.info().await {
                    Ok(info) => {
                        let systemd = matches!(
                            info.cgroup_driver,
                            Some(bollard::models::SystemInfoCgroupDriverEnum::SYSTEMD)
                        );
                        let v2 = matches!(
                            info.cgroup_version,
                            Some(bollard::models::SystemInfoCgroupVersionEnum::_2)
                        );
                        tracing::info!(
                            systemd,
                            cgroup_v2 = v2,
                            "workload slices {}",
                            if systemd && v2 { "enabled" } else { "unavailable on this daemon" }
                        );
                        systemd && v2
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "could not read the Docker daemon's cgroup driver; workload slices disabled");
                        false
                    }
                }
            })
            .await
    }

    async fn create_from_body(
        &self,
        spec: &InstanceSpec,
        mut body: ContainerCreateBody,
    ) -> Result<InstanceHandle, DriverError> {
        // Docker only inherits image CMD when no ENTRYPOINT override is sent.
        // Resolve it here for both custom and cleared entrypoints, keeping the
        // two overrides independent as promised by the workload contract.
        if spec.entrypoint.is_some() && spec.command.is_none() {
            let image = self
                .docker
                .inspect_image(&spec.image)
                .await
                .map_err(runtime("inspect image command", &spec.name))?;
            body.cmd = image.config.and_then(|config| config.cmd);
        }
        // Each workload's containers share a slice, so network policy can
        // identify them by cgroup from their first instruction (network
        // access plan, §4.15).
        if let Some(subject) = temps_workload::labels::policy_subject(spec.owner, &spec.labels) {
            let host = body.host_config.get_or_insert_with(Default::default);
            if host.cgroup_parent.is_none() && self.supports_slices().await {
                host.cgroup_parent = Some(temps_workload::labels::workload_slice(subject));
            }
        }
        let mut options = CreateContainerOptionsBuilder::new().name(&spec.name);
        if let Some(platform) = &spec.platform {
            options = options.platform(platform);
        }
        let created = self
            .docker
            .create_container(Some(options.build()), body)
            .await
            .map_err(runtime("create", &spec.name))?;
        Ok(InstanceHandle {
            driver: DRIVER.to_string(),
            id: created.id,
            name: spec.name.clone(),
        })
    }

    async fn connect(&self, network: &str, container: &str) -> Result<(), bollard::errors::Error> {
        self.docker
            .connect_network(
                network,
                NetworkConnectRequest {
                    container: container.to_string(),
                    endpoint_config: None,
                },
            )
            .await
    }

    /// Joins the attachment's extra networks. Required networks fail the
    /// create; optional ones are skipped when they do not exist.
    pub async fn attach_networks(
        &self,
        handle: &InstanceHandle,
        attach: &Attachments,
    ) -> Result<(), DriverError> {
        for network in &attach.network.optional {
            match self.connect(network, &handle.id).await {
                Ok(()) => {}
                Err(e) if is_not_found(&e) => {
                    tracing::debug!(network = %network, instance = %handle.name, "optional network absent, skipped");
                }
                Err(e) => {
                    return Err(DriverError::Attach {
                        driver: DRIVER.to_string(),
                        instance: handle.name.clone(),
                        network: network.clone(),
                        reason: e.to_string(),
                    })
                }
            }
        }
        for network in &attach.network.required {
            self.connect(network, &handle.id)
                .await
                .map_err(|e| DriverError::Attach {
                    driver: DRIVER.to_string(),
                    instance: handle.name.clone(),
                    network: network.clone(),
                    reason: e.to_string(),
                })?;
        }
        Ok(())
    }
}

#[async_trait]
impl RuntimeDriver for DockerDriver {
    fn capabilities(&self) -> &DriverCapabilities {
        &self.capabilities
    }

    async fn prepare(&self, artifact: &Artifact) -> Result<PreparedArtifact, DriverError> {
        let Artifact::OciImage { reference, digest } = artifact else {
            return Err(DriverError::Unsupported {
                driver: DRIVER.to_string(),
                operation: "non-OCI artifacts",
                instance: String::new(),
            });
        };
        let local_ref = match digest {
            Some(digest) => format!(
                "{}@{digest}",
                reference.split('@').next().unwrap_or(reference)
            ),
            None => reference.clone(),
        };
        if self.docker.inspect_image(&local_ref).await.is_err() {
            let options = CreateImageOptionsBuilder::new()
                .from_image(&local_ref)
                .build();
            self.docker
                .create_image(Some(options), None, None)
                .try_collect::<Vec<_>>()
                .await
                .map_err(|e| DriverError::Artifact {
                    driver: DRIVER.to_string(),
                    artifact: local_ref.clone(),
                    reason: e.to_string(),
                })?;
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
        let secrets = self.write_secret_files(spec).await?;
        let binds = secrets.iter().map(|(_, bind)| bind.clone()).collect();
        let handle = match self.create_with_binds(spec, attach, binds).await {
            Ok(handle) => handle,
            Err(error) => {
                if let Some((dir, _)) = &secrets {
                    secrets::remove_secrets_dir(dir);
                }
                return Err(error);
            }
        };
        if let Err(error) = self.attach_networks(&handle, attach).await {
            let _ = self.remove(&handle, true).await;
            return Err(error);
        }
        Ok(handle)
    }

    async fn start(&self, handle: &InstanceHandle) -> Result<(), DriverError> {
        self.docker
            .start_container(&handle.id, None::<StartContainerOptions>)
            .await
            .map_err(runtime("start", &handle.name))
    }

    async fn stop(&self, handle: &InstanceHandle) -> Result<(), DriverError> {
        match self
            .docker
            .stop_container(&handle.id, None::<StopContainerOptions>)
            .await
        {
            Ok(()) => Ok(()),
            Err(e) if is_not_found(&e) => Ok(()),
            // 304: already stopped.
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 304, ..
            }) => Ok(()),
            Err(e) => Err(runtime("stop", &handle.name)(e)),
        }
    }

    async fn remove(&self, handle: &InstanceHandle, purge: bool) -> Result<(), DriverError> {
        let options = RemoveContainerOptions {
            force: true,
            v: purge,
            ..Default::default()
        };
        let removed = match self
            .docker
            .remove_container(&handle.id, Some(options))
            .await
        {
            Ok(()) => Ok(()),
            Err(e) if is_not_found(&e) => Ok(()),
            Err(e) => Err(runtime("remove", &handle.name)(e)),
        };
        // The container is gone, so nothing holds its secrets any more.
        if removed.is_ok() {
            if let Some(dir) = self.secrets_dir_of(&handle.name) {
                secrets::remove_secrets_dir(&dir);
            }
        }
        removed
    }

    async fn inspect(&self, handle: &InstanceHandle) -> Result<InstanceStatus, DriverError> {
        let inspect = match self
            .docker
            .inspect_container(&handle.id, None::<InspectContainerOptions>)
            .await
        {
            Ok(inspect) => inspect,
            Err(e) if is_not_found(&e) => {
                return Ok(InstanceStatus {
                    state: InstanceState::Missing,
                    exit_code: None,
                    started_at: None,
                    finished_at: None,
                    address: None,
                    published_ports: Vec::new(),
                })
            }
            Err(e) => return Err(runtime("inspect", &handle.name)(e)),
        };
        let state = inspect.state.unwrap_or_default();
        let parse = |t: Option<String>| {
            t.and_then(|t| chrono::DateTime::parse_from_rfc3339(&t).ok())
                .map(|t| t.with_timezone(&chrono::Utc))
                // Docker reports never-set times as the zero time.
                .filter(|t| t.timestamp() > 0)
        };
        let running = state.running.unwrap_or(false);
        let started_at = parse(state.started_at);
        let instance_state = if running {
            InstanceState::Running
        } else if started_at.is_some() {
            InstanceState::Exited
        } else {
            InstanceState::Created
        };
        let settings = inspect.network_settings.unwrap_or_default();
        let address = settings.networks.as_ref().and_then(|n| {
            n.values()
                .find_map(|e| e.ip_address.clone().filter(|ip| !ip.is_empty()))
        });
        let mut published_ports = Vec::new();
        for (key, bindings) in settings.ports.unwrap_or_default() {
            let Some((port, proto)) = key.split_once('/') else {
                continue;
            };
            let (Ok(container_port), proto) = (port.parse::<u16>(), proto) else {
                continue;
            };
            let protocol = if proto == "udp" {
                PortProtocol::Udp
            } else {
                PortProtocol::Tcp
            };
            for binding in bindings.unwrap_or_default() {
                published_ports.push(temps_workload::PortBinding {
                    container_port,
                    protocol,
                    host_ip: binding.host_ip.unwrap_or_default(),
                    host_port: binding.host_port.and_then(|p| p.parse().ok()),
                });
            }
        }
        published_ports.sort_by_key(|p| (p.container_port, p.host_ip.clone()));
        Ok(InstanceStatus {
            state: instance_state,
            exit_code: (!running).then_some(state.exit_code).flatten(),
            started_at,
            finished_at: parse(state.finished_at),
            address,
            published_ports,
        })
    }

    async fn logs(
        &self,
        handle: &InstanceHandle,
        query: LogQuery,
    ) -> Result<LogStream, DriverError> {
        let options = LogsOptions {
            stdout: true,
            stderr: true,
            follow: query.follow,
            since: query.since.map(|t| t.timestamp() as i32).unwrap_or(0),
            tail: query
                .tail
                .map(|n| n.to_string())
                .unwrap_or_else(|| "all".to_string()),
            ..Default::default()
        };
        let name = handle.name.clone();
        let stream = self
            .docker
            .logs(&handle.id, Some(options))
            .map(move |chunk| {
                let chunk = chunk.map_err(|e| DriverError::Runtime {
                    driver: DRIVER.to_string(),
                    operation: "logs",
                    instance: name.clone(),
                    reason: e.to_string(),
                })?;
                let stderr = matches!(chunk, bollard::container::LogOutput::StdErr { .. });
                // Docker's own timestamps are not requested here; the
                // container log path asks Docker for them directly.
                Ok(LogLine {
                    stderr,
                    line: chunk.into_bytes(),
                    timestamp: None,
                })
            });
        Ok(Box::pin(stream))
    }

    async fn stats(&self, handle: &InstanceHandle) -> Result<InstanceStats, DriverError> {
        let options = StatsOptions {
            stream: false,
            one_shot: true,
        };
        let mut stream = self.docker.stats(&handle.id, Some(options));
        let stats = match stream.next().await {
            Some(stats) => stats.map_err(runtime("stats", &handle.name))?,
            None => return Ok(InstanceStats::default()),
        };
        Ok(InstanceStats {
            cpu_nanos: stats
                .cpu_stats
                .and_then(|c| c.cpu_usage)
                .and_then(|u| u.total_usage),
            memory_bytes: stats.memory_stats.and_then(|m| m.usage),
            pids: stats.pids_stats.and_then(|p| p.current),
            ..InstanceStats::default()
        })
    }

    async fn list(
        &self,
        labels: &std::collections::BTreeMap<String, String>,
    ) -> Result<Vec<ListedInstance>, DriverError> {
        let options = ListContainersOptions {
            all: true,
            filters: Some(HashMap::from([(
                "label".to_string(),
                labels.iter().map(|(k, v)| format!("{k}={v}")).collect(),
            )])),
            ..Default::default()
        };
        let containers = self
            .docker
            .list_containers(Some(options))
            .await
            .map_err(runtime("list", "*"))?;
        Ok(containers
            .into_iter()
            .filter_map(|c| {
                let id = c.id?;
                let name = c
                    .names
                    .and_then(|names| names.into_iter().next())
                    .map(|n| n.trim_start_matches('/').to_string())
                    .unwrap_or_else(|| id.clone());
                Some(ListedInstance {
                    handle: InstanceHandle {
                        driver: DRIVER.to_string(),
                        id,
                        name,
                    },
                    labels: c.labels.unwrap_or_default().into_iter().collect(),
                })
            })
            .collect())
    }

    async fn wait(&self, handle: &InstanceHandle) -> Result<(), DriverError> {
        let options = WaitContainerOptions {
            condition: "not-running".to_string(),
        };
        let mut stream = self.docker.wait_container(&handle.id, Some(options));
        match stream.next().await {
            // A non-zero exit arrives as an error carrying the code; either
            // way the instance has stopped running.
            None
            | Some(Ok(_))
            | Some(Err(bollard::errors::Error::DockerContainerWaitError { .. })) => Ok(()),
            Some(Err(e)) if is_not_found(&e) => Ok(()),
            Some(Err(e)) => Err(runtime("wait", &handle.name)(e)),
        }
    }
}

/// Docker's image-config merge treats [] as unspecified. [""] explicitly
/// removes the image ENTRYPOINT (Engine API ContainerCreateBody contract).
fn docker_entrypoint_override(entrypoint: Option<&[String]>) -> Option<Vec<String>> {
    entrypoint.map(|args| {
        if args.is_empty() {
            vec![String::new()]
        } else {
            args.to_vec()
        }
    })
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn entrypoint_override_inherits_image_command() {
        use serde_json::json;
        use temps_docker_capture::{FakeDocker, Method};

        for entrypoint in [vec![], vec!["/custom-entrypoint".into()]] {
            for command in [None, Some(vec!["explicit-command".into()])] {
                let fake = FakeDocker::start().await.unwrap();
                fake.allow_create("shop-production-31");
                fake.respond(
                    Method::GET,
                    "/images/",
                    200,
                    json!({"Config": {"Cmd": ["image-command", "argument with spaces"]}}),
                );
                let mut s = spec();
                s.entrypoint = Some(entrypoint.clone());
                s.command = command.clone();
                DockerDriver::new(fake.docker())
                    .create(&s, &attach())
                    .await
                    .unwrap();
                let creates = fake.creates();
                assert_eq!(creates[0].body["Image"], s.image);
                assert_eq!(
                    creates[0].body["Cmd"],
                    command.map_or(
                        json!(["image-command", "argument with spaces"]),
                        |cmd| json!(cmd)
                    )
                );
                assert_eq!(
                    fake.calls()
                        .iter()
                        .filter(|c| c.path.starts_with("/images/"))
                        .count(),
                    usize::from(s.command.is_none()),
                    "only inspect when inheriting CMD with an entrypoint override"
                );
            }
        }
    }

    #[tokio::test]
    async fn entrypoint_override_stops_before_create_if_image_inspection_fails() {
        let fake = temps_docker_capture::FakeDocker::start().await.unwrap();
        fake.allow_create("shop-production-31");
        let root = tempfile::tempdir().unwrap();
        let mut s = spec();
        s.entrypoint = Some(vec![]);
        s.secret_files.push(temps_workload::SecretFile {
            name: "API_TOKEN".into(),
            value: "seeded-token-value".into(),
        });
        let error = DockerDriver::new(fake.docker())
            .with_secrets_root(root.path())
            .create(&s, &attach())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("inspect"), "{error}");
        assert!(
            !root.path().join(&s.name).exists(),
            "failed inspection must clean up secrets"
        );
        assert!(
            fake.creates().is_empty(),
            "do not start with a silently missing command"
        );
    }

    #[test]
    fn dockerfile_entrypoint_inheritance_override_and_reset() {
        for (entrypoint, expected) in [
            (None, None),
            (Some(vec![]), Some(vec![String::new()])),
            (
                Some(vec!["/bin/sh".into(), "-c".into()]),
                Some(vec!["/bin/sh".into(), "-c".into()]),
            ),
        ] {
            let mut spec = spec();
            spec.command = Some(vec!["printf '%s' 'argument with spaces'".into()]);
            spec.entrypoint = entrypoint;
            let body = container_create_body(&spec, &attach(), vec![]);
            assert_eq!(body.cmd, spec.command);
            assert_eq!(body.entrypoint, expected);
        }
    }

    use super::*;
    use std::collections::BTreeMap;
    use temps_workload::{
        InstanceLimits, NetworkAttachment, PortBinding, SecurityProfile, WorkloadKind, WorkloadRef,
    };

    fn spec() -> InstanceSpec {
        InstanceSpec {
            owner: WorkloadRef::new(WorkloadKind::Deployment, 31),
            name: "shop-production-31".into(),
            image: "registry.example.test/shop/web:4f2a9c1".into(),
            platform: None,
            env: vec![("PORT".into(), "3000".into())],
            secret_files: vec![],
            command: None,
            entrypoint: None,
            workdir: None,
            user: None,
            ports: vec![PortBinding {
                container_port: 3000,
                protocol: PortProtocol::Tcp,
                host_ip: "127.0.0.1".into(),
                host_port: None,
            }],
            restart: RestartPolicy::OnFailure,
            limits: InstanceLimits {
                nano_cpus: Some(500_000_000),
                memory_bytes: Some(512 * 1024 * 1024),
                memory_swap_bytes: Some(512 * 1024 * 1024),
                pids: None,
                disk_mb: None,
                ..InstanceLimits::default()
            },
            log_rotation: Some(LogRotation {
                max_size: "50m".into(),
                max_file: 3,
            }),
            labels: BTreeMap::from([("sh.temps.deploy_id".into(), "31".into())]),
            security: SecurityProfile::Trusted,
        }
    }

    fn attach() -> Attachments {
        Attachments {
            network: NetworkAttachment {
                primary: Some("temps-app-network".into()),
                ..Default::default()
            },
            volumes: vec!["temps-vol-1:/data".into()],
        }
    }

    #[test]
    fn the_body_is_hardened_and_carries_the_spec() {
        let body = container_create_body(
            &spec(),
            &attach(),
            vec!["/srv/secrets/x:/run/secrets:ro".into()],
        );
        let host = body.host_config.as_ref().unwrap();
        assert_eq!(host.cap_drop, Some(vec!["ALL".into()]));
        assert_eq!(
            host.security_opt,
            Some(vec!["no-new-privileges:true".into()])
        );
        assert_eq!(host.pids_limit, Some(DEFAULT_PIDS_LIMIT));
        assert_eq!(host.init, Some(true));
        assert_eq!(
            host.binds,
            Some(vec![
                "/srv/secrets/x:/run/secrets:ro".into(),
                "temps-vol-1:/data".into()
            ]),
            "caller binds first, volumes after; none dropped"
        );
        assert_eq!(host.network_mode.as_deref(), Some("temps-app-network"));
        assert_eq!(host.memory, host.memory_swap, "swap is disabled");
        assert_eq!(body.env, Some(vec!["PORT=3000".into()]));
        assert_eq!(body.exposed_ports, Some(vec!["3000/tcp".into()]));
        let binding = &host.port_bindings.as_ref().unwrap()["3000/tcp"]
            .as_ref()
            .unwrap()[0];
        assert_eq!(binding.host_ip.as_deref(), Some("127.0.0.1"));
        assert_eq!(binding.host_port, None, "Docker picks the port");
    }

    #[test]
    fn a_spec_pids_cap_overrides_the_default_and_empty_labels_are_omitted() {
        let mut s = spec();
        s.limits.pids = Some(2048);
        s.labels.clear();
        let body = container_create_body(&s, &Attachments::default(), vec![]);
        assert_eq!(body.host_config.as_ref().unwrap().pids_limit, Some(2048));
        assert_eq!(body.labels, None);
        assert_eq!(body.host_config.as_ref().unwrap().binds, None);
    }

    #[test]
    fn secret_values_never_reach_the_request() {
        let mut s = spec();
        s.secret_files.push(temps_workload::SecretFile {
            name: "API_TOKEN".into(),
            value: "seeded-token-value".into(),
        });
        let body = container_create_body(&s, &attach(), vec![]);
        let json = serde_json::to_string(&body).unwrap();
        assert!(!json.contains("seeded-token-value"), "{json}");
    }

    #[test]
    fn restart_policies_map_one_to_one() {
        assert_eq!(
            restart_policy_name(RestartPolicy::Never),
            RestartPolicyNameEnum::NO
        );
        assert_eq!(
            restart_policy_name(RestartPolicy::Always),
            RestartPolicyNameEnum::ALWAYS
        );
        assert_eq!(
            restart_policy_name(RestartPolicy::OnFailure),
            RestartPolicyNameEnum::ON_FAILURE
        );
        assert_eq!(
            restart_policy_name(RestartPolicy::UnlessStopped),
            RestartPolicyNameEnum::UNLESS_STOPPED
        );
    }

    #[tokio::test]
    async fn secret_files_are_written_privately_and_bound_read_only() {
        let fake = temps_docker_capture::FakeDocker::start().await.unwrap();
        fake.allow_create("shop-production-31");
        let root = tempfile::tempdir().unwrap();
        let driver = DockerDriver::new(fake.docker()).with_secrets_root(root.path());
        let mut s = spec();
        s.secret_files.push(temps_workload::SecretFile {
            name: "API_TOKEN".into(),
            value: "seeded-token-value".into(),
        });
        let handle = driver.create(&s, &attach()).await.expect("created");

        let dir = root.path().join("shop-production-31");
        assert_eq!(
            std::fs::read_to_string(dir.join("API_TOKEN")).unwrap(),
            "seeded-token-value"
        );
        let create = &fake.creates()[0];
        let body = serde_json::to_string(&create.body).unwrap();
        assert!(
            body.contains(&format!("{}:/run/secrets:ro", dir.display())),
            "{body}"
        );
        assert!(!body.contains("seeded-token-value"), "{body}");

        driver.remove(&handle, true).await.expect("removed");
        assert!(!dir.exists(), "removing the instance removes its secrets");
    }

    #[tokio::test]
    async fn a_failed_create_leaves_no_secrets_behind() {
        let fake = temps_docker_capture::FakeDocker::start().await.unwrap();
        let root = tempfile::tempdir().unwrap();
        let driver = DockerDriver::new(fake.docker()).with_secrets_root(root.path());
        let mut s = spec();
        s.secret_files.push(temps_workload::SecretFile {
            name: "API_TOKEN".into(),
            value: "v".into(),
        });
        driver.create(&s, &attach()).await.unwrap_err();
        assert!(!root.path().join("shop-production-31").exists());
    }

    #[tokio::test]
    async fn the_trait_create_refuses_secret_files_it_cannot_place() {
        let fake = temps_docker_capture::FakeDocker::start().await.unwrap();
        let driver = DockerDriver::new(fake.docker());
        let mut s = spec();
        s.secret_files.push(temps_workload::SecretFile {
            name: "API_TOKEN".into(),
            value: "v".into(),
        });
        let err = driver.create(&s, &attach()).await.unwrap_err();
        assert!(matches!(err, DriverError::Secrets { .. }), "{err}");
        assert!(fake.creates().is_empty(), "nothing reached Docker");
    }

    #[tokio::test]
    async fn create_errors_name_the_instance_and_the_driver() {
        let fake = temps_docker_capture::FakeDocker::start().await.unwrap();
        let driver = DockerDriver::new(fake.docker());
        let err = driver.create(&spec(), &attach()).await.unwrap_err();
        let text = err.to_string();
        assert!(
            text.starts_with("driver 'docker' failed to create instance 'shop-production-31'"),
            "{text}"
        );
        assert_eq!(fake.creates().len(), 1);
        assert_eq!(
            fake.creates()[0].name.as_deref(),
            Some("shop-production-31")
        );
    }
}
