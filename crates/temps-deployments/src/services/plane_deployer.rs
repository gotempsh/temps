// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! A [`ContainerDeployer`] that creates a deployment's replicas through the
//! workload plane (ADR-055 Phase 5, `workload_mode = enforce`): on the
//! control plane, or pinned to the worker the deploy job placed it on.
//!
//! The deploy job keeps everything that makes a deployment a deployment:
//! the health check, blue/green promotion, the `deployment_containers` row,
//! retirement of the previous revision. Only the create goes through the
//! plane: [`ContainerDeployer::deploy_container`] applies one `Deployment`
//! workload per replica, waits for its `Running` condition, and answers with
//! the container the plane started. Every other operation acts on that
//! container through the ordinary local deployer, exactly as for a legacy
//! replica, so the product's lifecycle code needs no second path.
//!
//! The deployment's variables and secret files are staged in memory
//! ([`WorkloadPlane::stage_env_with_files`]) and never written into the
//! stored spec; the driver writes the files under the same secrets root the
//! legacy deployer uses and mounts them read-only at `/run/secrets`.
//!
//! The ADR-045 Docker socket is decided by admission, from this process's
//! own grant: a granted project's replica is admitted `host_privileged` and
//! gets exactly the socket bind, nothing else relaxed.
//!
//! ADR-055 Phase 10: an environment whose `workload_isolation` is
//! `micro_vm` asks for [`Isolation::MicroVm`]. The plane places it only on a
//! node whose driver provides it (refused with the scheduler's reason
//! otherwise), and a microVM is not a container the local Docker can act
//! on, so every operation on such a replica goes through the plane
//! (inspect, logs, exec, retire) instead of the local deployer.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use temps_deployer::{
    ContainerDeployer, ContainerInfo, ContainerStatus, DeployRequest, DeployResult, DeployerError,
};
use temps_workload::{
    Artifact, ConditionKind, EnvSpec, Isolation, Lifetime, NetworkSpec, PlacementSpec,
    PortProtocol, PortSpec, Process, ResourceAmounts, Resources, RuntimeDriver, SecretFile,
    SecurityProfile, WorkloadKind, WorkloadRef, WorkloadSpec,
};
use temps_workload_plane::scheduler::CONTROL_PLANE_NODE_ID;
use temps_workload_plane::{ApplyRequest, Owner, WorkloadPlane};

/// Owner kind of a deployment's workloads.
pub const DEPLOYMENT_OWNER_KIND: &str = "deployment";

/// The control plane's microVM runtime (ADR-055 Phase 10), registered by
/// the agents plugin when this host has a working Firecracker. The
/// deployments plugin adds it to the workload plane; without it a
/// `micro_vm` replica is refused with the scheduler's reason.
pub struct ControlPlaneMicroVmDriver(pub Arc<dyn RuntimeDriver>);

/// How many log lines a microVM replica's log read returns.
const MICRO_VM_LOG_TAIL: u32 = 500;

/// The `Deployment` workload one replica's create request becomes: pinned
/// to `node_id` (the job already placed it there) with `isolation`, long-running
/// with the request's restart policy, its variables imported from
/// `staged_import`, and its CPU/memory caps carried over as limits.
pub fn deployment_workload_spec(
    request: &DeployRequest,
    deployment_id: i32,
    node_id: i32,
    staged_import: String,
    isolation: Isolation,
) -> Result<WorkloadSpec, DeployerError> {
    // A microVM runs on its own TAP device and kernel: the Firecracker
    // driver joins no Docker network and mounts neither `/run/secrets` nor
    // the host Docker socket, so a replica that needs one is refused here
    // rather than started without it.
    if isolation == Isolation::MicroVm {
        if !request.extra_networks.is_empty() {
            return Err(DeployerError::DeploymentFailed(format!(
                "Container {} must join Docker network(s) {}, which a microVM cannot: it runs on \
                 its own TAP device. Set this environment's workload_isolation back to process to \
                 deploy it",
                request.container_name,
                request.extra_networks.join(", ")
            )));
        }
        if !request.secrets.is_empty() {
            return Err(DeployerError::DeploymentFailed(format!(
                "Container {} has {} secret file(s), which the microVM runtime cannot mount at \
                 /run/secrets yet. Set this environment's workload_isolation back to process to \
                 deploy it",
                request.container_name,
                request.secrets.len()
            )));
        }
        if request.control_plane_grants_socket {
            return Err(DeployerError::DeploymentFailed(format!(
                "Container {} is for project '{}', which is granted the host Docker socket (ADR \
                 045); a microVM cannot mount it. Set this environment's workload_isolation back \
                 to process to deploy it",
                request.container_name,
                request.project_slug.as_deref().unwrap_or("<unknown>")
            )));
        }
    }
    let restart = match request.restart_policy {
        temps_deployer::RestartPolicy::Never => temps_workload::RestartPolicy::Never,
        temps_deployer::RestartPolicy::Always => temps_workload::RestartPolicy::Always,
        temps_deployer::RestartPolicy::OnFailure => temps_workload::RestartPolicy::OnFailure,
        temps_deployer::RestartPolicy::UnlessStopped => {
            temps_workload::RestartPolicy::UnlessStopped
        }
    };
    let limits = ResourceAmounts {
        cpu_millis: request
            .resource_limits
            .cpu_limit
            .filter(|cores| *cores > 0.0)
            .map(|cores| (cores * 1000.0).ceil() as u32),
        // 0 is the deploy path's "explicitly uncapped".
        memory_mb: request
            .resource_limits
            .memory_limit_mb
            .filter(|mb| *mb > 0)
            .map(|mb| u32::try_from(mb).unwrap_or(u32::MAX)),
        disk_mb: None,
        pids: None,
    };
    Ok(WorkloadSpec {
        owner: WorkloadRef::new(WorkloadKind::Deployment, i64::from(deployment_id)),
        artifact: Artifact::oci(request.image_name.clone()),
        isolation,
        process: Process {
            command: request.command.clone(),
            ..Default::default()
        },
        env: EnvSpec {
            imports: vec![staged_import],
            ..Default::default()
        },
        resources: Resources {
            requests: None,
            limits: (!limits.is_empty()).then_some(limits),
        },
        volumes: Vec::new(),
        ports: request
            .port_mappings
            .iter()
            .map(|mapping| PortSpec {
                name: format!("port-{}", mapping.container_port),
                container_port: mapping.container_port,
                protocol: match mapping.protocol {
                    temps_deployer::Protocol::Tcp => PortProtocol::Tcp,
                    temps_deployer::Protocol::Udp => PortProtocol::Udp,
                },
                app_protocol: Default::default(),
            })
            .collect(),
        network: NetworkSpec {
            egress: None,
            extra_networks: request.extra_networks.clone(),
        },
        probes: Default::default(),
        placement: PlacementSpec {
            nodes: Some(vec![node_id]),
            labels: None,
            spread: false,
            replicas: 1,
        },
        lifetime: Lifetime::LongRunning { restart },
        security: None,
        labels: request
            .labels
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect::<BTreeMap<_, _>>(),
    })
}

/// Creates one deployment's replicas on one node through the plane.
pub struct PlaneContainerDeployer {
    plane: Arc<WorkloadPlane>,
    /// The deployer of the node the replicas run on (the local one, or the
    /// worker's `/agent/*` client), for every operation after the create.
    local: Arc<dyn ContainerDeployer>,
    deployment_id: i32,
    /// Where the replicas are pinned: the control plane by default.
    node_id: i32,
    ready_timeout: Duration,
    /// What the replicas this deployer creates ask for.
    isolation: Isolation,
    /// Container id -> workload id, for retiring a replica through the
    /// plane. One deploy job's replicas: a handful of entries.
    workloads: Mutex<HashMap<String, i64>>,
    /// The environment's attached volumes and their owner (ADR-055 Phase
    /// 9). Empty: the replica mounts nothing, exactly as before.
    volumes: Vec<temps_workload::VolumeMount>,
    volume_owner: Option<Owner>,
}

impl PlaneContainerDeployer {
    pub fn new(
        plane: Arc<WorkloadPlane>,
        local: Arc<dyn ContainerDeployer>,
        deployment_id: i32,
        ready_timeout: Duration,
    ) -> Self {
        Self {
            plane,
            local,
            deployment_id,
            node_id: CONTROL_PLANE_NODE_ID,
            ready_timeout,
            isolation: Isolation::Process,
            workloads: Mutex::new(HashMap::new()),
            volumes: Vec::new(),
            volume_owner: None,
        }
    }

    /// Pin the replicas to worker `node_id`; `local` must then be that
    /// worker's deployer.
    pub fn on_node(mut self, node_id: i32) -> Self {
        self.node_id = node_id;
        self
    }

    /// Mount `volumes` of `owner` in every replica this deployer starts.
    pub fn with_volumes(mut self, volumes: Vec<temps_workload::VolumeMount>, owner: Owner) -> Self {
        if !volumes.is_empty() {
            self.volumes = volumes;
            self.volume_owner = Some(owner);
        }
        self
    }

    /// Ask for `isolation` (the environment's `workload_isolation`).
    pub fn with_isolation(mut self, isolation: Isolation) -> Self {
        self.isolation = isolation;
        self
    }

    /// The plane workload behind `container_id` when its runtime is not the
    /// local Docker (a microVM), so the caller must go through the plane.
    /// `None` for a container: the local deployer serves it as before.
    async fn plane_only_workload(&self, container_id: &str) -> Result<Option<i64>, DeployerError> {
        let found = self
            .plane
            .workload_for_instance(container_id)
            .await
            .map_err(|e| self.plane_error(&format!("look up instance {container_id}"), e))?;
        Ok(match found {
            Some((workload_id, driver)) if driver != temps_workload_docker::DRIVER => {
                Some(workload_id)
            }
            _ => None,
        })
    }

    async fn plane_logs(
        &self,
        workload_id: i64,
        container_id: &str,
    ) -> Result<String, DeployerError> {
        let lines = self
            .plane
            .instance_logs(workload_id, MICRO_VM_LOG_TAIL)
            .await
            .map_err(|e| self.plane_error(&format!("read the logs of {container_id}"), e))?
            .unwrap_or_default();
        Ok(lines
            .iter()
            .map(|line| String::from_utf8_lossy(&line.line).into_owned())
            .collect())
    }

    fn unsupported_for_micro_vm(&self, operation: &str, container_id: &str) -> DeployerError {
        DeployerError::DeploymentFailed(format!(
            "Cannot {operation} replica {container_id} of deployment {}: it runs in a microVM, \
             which supports inspect, logs, exec and removal only",
            self.deployment_id
        ))
    }

    fn plane_error(&self, operation: &str, error: impl std::fmt::Display) -> DeployerError {
        DeployerError::DeploymentFailed(format!(
            "Workload plane could not {operation} for deployment {}: {error}",
            self.deployment_id
        ))
    }
}

#[async_trait]
impl ContainerDeployer for PlaneContainerDeployer {
    async fn deploy_container(
        &self,
        request: DeployRequest,
    ) -> Result<DeployResult, DeployerError> {
        let staged = self.plane.stage_env_with_files(
            &format!(
                "deployment-{}-{}",
                self.deployment_id, request.container_name
            ),
            request
                .environment_vars
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
            request
                .secrets
                .iter()
                .map(|(name, value)| SecretFile {
                    name: name.clone(),
                    value: value.clone(),
                })
                .collect(),
        );
        if self.isolation == Isolation::MicroVm && !self.volumes.is_empty() {
            return Err(DeployerError::DeploymentFailed(format!(
                "Container {} mounts volume(s) {}, which the microVM runtime cannot mount yet. Set \
                 this environment's workload_isolation back to process to deploy it",
                request.container_name,
                self.volumes
                    .iter()
                    .map(|mount| mount.volume.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        let mut spec = deployment_workload_spec(
            &request,
            self.deployment_id,
            self.node_id,
            staged.import(),
            self.isolation,
        )?;
        spec.volumes = self.volumes.clone();
        let container_port = request
            .port_mappings
            .first()
            .map(|mapping| mapping.container_port)
            .unwrap_or(0);
        let apply = ApplyRequest {
            owner: Owner::new(DEPLOYMENT_OWNER_KIND, i64::from(self.deployment_id)),
            name: request.container_name.clone(),
            spec,
            project_slug: request.project_slug.clone(),
            finalizers: Vec::new(),
        };
        let start = |e: temps_workload_plane::PlaneError| {
            self.plane_error(&format!("start {}", request.container_name), e)
        };
        let view = match self.volume_owner.as_ref() {
            None => self
                .plane
                .apply_and_wait(apply, ConditionKind::Running, self.ready_timeout)
                .await
                .map_err(start)?,
            Some(owner) => {
                let applied = self
                    .plane
                    .apply_with_volumes(apply, owner)
                    .await
                    .map_err(start)?;
                if let Some(pin) = applied.pin.as_ref() {
                    tracing::info!(
                        deployment_id = self.deployment_id,
                        workload_id = applied.workload_id,
                        "deployment replica {pin}"
                    );
                }
                self.plane
                    .wait_for(
                        applied.workload_id,
                        ConditionKind::Running,
                        self.ready_timeout,
                    )
                    .await
                    .map_err(start)?
            }
        };
        // The values were only needed for the create.
        drop(staged);
        let (handle, status) = self
            .plane
            .instance_status(view.id)
            .await
            .map_err(|e| self.plane_error("inspect the started replica", e))?
            .ok_or_else(|| {
                self.plane_error(
                    "find the started replica",
                    format!("workload {} reports Running but has no instance", view.id),
                )
            })?;
        let host_port = status
            .published_ports
            .iter()
            .find(|binding| binding.container_port == container_port)
            .and_then(|binding| binding.host_port)
            .unwrap_or(0);
        if let Ok(mut workloads) = self.workloads.lock() {
            workloads.insert(handle.id.clone(), view.id);
        }
        let docker_socket_mounted = view.spec.security == Some(SecurityProfile::HostPrivileged);
        if docker_socket_mounted {
            // The same host-local record the legacy deployer writes (ADR 045).
            tracing::warn!(
                event = "docker_socket_mounted",
                container_name = %handle.name,
                project_slug = request.project_slug.as_deref().unwrap_or("<unknown>"),
                "Mounted the host Docker socket into this container through the workload plane: \
                 the project is named in {}. It is root-equivalent on this host.",
                temps_core::docker_socket_grant::DOCKER_SOCKET_PROJECTS_ENV
            );
        }
        tracing::info!(
            deployment_id = self.deployment_id,
            workload_id = view.id,
            container_id = %handle.id,
            driver = %handle.driver,
            isolation = temps_workload_plane::scheduler::isolation_name(self.isolation),
            "deployment replica started through the workload plane"
        );
        Ok(DeployResult {
            container_id: handle.id,
            container_name: handle.name,
            container_port,
            host_port,
            status: ContainerStatus::Running,
            docker_socket_mounted,
        })
    }

    async fn exec_in_container(
        &self,
        container_id: &str,
        command: Vec<String>,
        timeout: Duration,
    ) -> Result<temps_deployer::ExecResult, DeployerError> {
        let Some(workload_id) = self.plane_only_workload(container_id).await? else {
            return self
                .local
                .exec_in_container(container_id, command, timeout)
                .await;
        };
        let exec = self.plane.instance_exec(
            workload_id,
            temps_workload::ExecSpec {
                command,
                env: Vec::new(),
                workdir: None,
            },
        );
        let lines = tokio::time::timeout(timeout, exec).await.map_err(|_| {
            self.plane_error(
                &format!("exec in {container_id}"),
                format!("timed out after {timeout:?}"),
            )
        })?;
        // A failing command is the stream's last item: report it as a
        // non-zero exit, like a container's exec.
        let (exit_code, lines) = match lines {
            Ok(lines) => (0, lines.unwrap_or_default()),
            Err(error) => {
                tracing::debug!(container_id, %error, "microVM exec failed");
                (1, Vec::new())
            }
        };
        let mut output = String::new();
        let mut truncated = false;
        for line in &lines {
            let text = String::from_utf8_lossy(&line.line);
            if output.len() + text.len() > temps_deployer::MAX_EXEC_OUTPUT_BYTES {
                truncated = true;
                break;
            }
            output.push_str(&text);
        }
        Ok(temps_deployer::ExecResult {
            exit_code,
            output,
            truncated,
        })
    }

    async fn start_container(&self, container_id: &str) -> Result<(), DeployerError> {
        if self.plane_only_workload(container_id).await?.is_some() {
            // The plane keeps a long-running replica running.
            return Ok(());
        }
        self.local.start_container(container_id).await
    }

    /// A microVM replica is stopped by retiring its workload: the plane
    /// does not keep a stopped instance around.
    async fn stop_container(&self, container_id: &str) -> Result<(), DeployerError> {
        if let Some(workload_id) = self.plane_only_workload(container_id).await? {
            return self.retire(workload_id, container_id).await;
        }
        self.local.stop_container(container_id).await
    }

    async fn pause_container(&self, container_id: &str) -> Result<(), DeployerError> {
        if self.plane_only_workload(container_id).await?.is_some() {
            return Err(self.unsupported_for_micro_vm("pause", container_id));
        }
        self.local.pause_container(container_id).await
    }

    async fn resume_container(&self, container_id: &str) -> Result<(), DeployerError> {
        if self.plane_only_workload(container_id).await?.is_some() {
            return Err(self.unsupported_for_micro_vm("resume", container_id));
        }
        self.local.resume_container(container_id).await
    }

    /// Retire through the plane when this deployer started it (the plane
    /// removes the instance), else remove it directly. A microVM is only
    /// ever removed by the plane.
    async fn remove_container(&self, container_id: &str) -> Result<(), DeployerError> {
        let workload = self
            .workloads
            .lock()
            .ok()
            .and_then(|mut workloads| workloads.remove(container_id));
        if let Some(workload_id) = workload {
            self.retire(workload_id, container_id).await?;
        }
        if let Some(workload_id) = self.plane_only_workload(container_id).await? {
            return self.retire(workload_id, container_id).await;
        }
        self.local.remove_container(container_id).await
    }

    async fn get_container_info(&self, container_id: &str) -> Result<ContainerInfo, DeployerError> {
        let Some(workload_id) = self.plane_only_workload(container_id).await? else {
            return self.local.get_container_info(container_id).await;
        };
        let view =
            self.plane.get(workload_id).await.map_err(|e| {
                self.plane_error(&format!("read the workload of {container_id}"), e)
            })?;
        let (handle, status) = self
            .plane
            .instance_status(workload_id)
            .await
            .map_err(|e| self.plane_error(&format!("inspect {container_id}"), e))?
            .ok_or_else(|| {
                self.plane_error(
                    &format!("inspect {container_id}"),
                    format!("workload {workload_id} has no instance"),
                )
            })?;
        Ok(micro_vm_container_info(&view.spec, &handle, &status))
    }

    async fn get_container_stats(
        &self,
        container_id: &str,
    ) -> Result<temps_deployer::ContainerStats, DeployerError> {
        if self.plane_only_workload(container_id).await?.is_some() {
            return Err(self.unsupported_for_micro_vm("read resource usage of", container_id));
        }
        self.local.get_container_stats(container_id).await
    }

    async fn list_containers(&self) -> Result<Vec<ContainerInfo>, DeployerError> {
        self.local.list_containers().await
    }

    async fn get_container_logs(&self, container_id: &str) -> Result<String, DeployerError> {
        if let Some(workload_id) = self.plane_only_workload(container_id).await? {
            return self.plane_logs(workload_id, container_id).await;
        }
        self.local.get_container_logs(container_id).await
    }

    /// A microVM's logs are read, not followed: the stream is the current
    /// tail, then ends.
    async fn stream_container_logs(
        &self,
        container_id: &str,
    ) -> Result<Box<dyn futures::Stream<Item = String> + Unpin + Send>, DeployerError> {
        if let Some(workload_id) = self.plane_only_workload(container_id).await? {
            let text = self.plane_logs(workload_id, container_id).await?;
            let lines: Vec<String> = text.lines().map(str::to_string).collect();
            return Ok(Box::new(futures::stream::iter(lines)));
        }
        self.local.stream_container_logs(container_id).await
    }

    async fn image_exists(&self, image_name: &str) -> Result<bool, DeployerError> {
        self.local.image_exists(image_name).await
    }
}

impl PlaneContainerDeployer {
    async fn retire(&self, workload_id: i64, container_id: &str) -> Result<(), DeployerError> {
        self.plane
            .delete(
                workload_id,
                "Retired",
                &format!(
                    "deployment {} removed container {container_id}",
                    self.deployment_id
                ),
            )
            .await
            .map_err(|e| self.plane_error("retire the replica", e))
    }
}

/// What a microVM replica looks like to code that reads container info.
fn micro_vm_container_info(
    spec: &WorkloadSpec,
    handle: &temps_workload::InstanceHandle,
    status: &temps_workload::InstanceStatus,
) -> ContainerInfo {
    let image_name = match &spec.artifact {
        Artifact::OciImage { reference, .. } => reference.clone(),
        other => format!("{other:?}"),
    };
    ContainerInfo {
        container_id: handle.id.clone(),
        container_name: handle.name.clone(),
        image_name,
        status: match status.state {
            temps_workload::InstanceState::Created => ContainerStatus::Created,
            temps_workload::InstanceState::Running => ContainerStatus::Running,
            temps_workload::InstanceState::Exited => ContainerStatus::Exited,
            temps_workload::InstanceState::Missing => ContainerStatus::Dead,
        },
        created_at: status.started_at.unwrap_or_else(chrono::Utc::now),
        ports: status
            .published_ports
            .iter()
            .filter_map(|binding| {
                Some(temps_deployer::PortMapping {
                    host_port: binding.host_port?,
                    container_port: binding.container_port,
                    protocol: match binding.protocol {
                        PortProtocol::Tcp => temps_deployer::Protocol::Tcp,
                        PortProtocol::Udp => temps_deployer::Protocol::Udp,
                    },
                    host_ip: Some(binding.host_ip.clone()),
                })
            })
            .collect(),
        // Never read back: the values live in the VM only.
        environment_vars: HashMap::new(),
        restart_count: None,
        labels: spec
            .labels
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
        exit_code: status.exit_code.and_then(|code| i32::try_from(code).ok()),
        exit_reason: status.exit_code.map(|code| format!("Exit code {code}")),
        oom_killed: None,
        error_message: None,
        finished_at: status.finished_at,
        started_at: status.started_at,
        cpu_limit_cores: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> DeployRequest {
        DeployRequest {
            image_name: "registry.example.test/shop/app:42".into(),
            container_name: "app-production-42-1".into(),
            environment_vars: HashMap::from([("PORT".into(), "3000".into())]),
            secrets: HashMap::new(),
            port_mappings: vec![temps_deployer::PortMapping {
                host_port: 0,
                container_port: 3000,
                protocol: temps_deployer::Protocol::Tcp,
                host_ip: None,
            }],
            network_name: Some("temps".into()),
            extra_networks: vec!["shop_default".into()],
            resource_limits: temps_deployer::ResourceLimits {
                cpu_limit: Some(0.5),
                memory_limit_mb: Some(0),
                disk_limit_mb: None,
            },
            restart_policy: temps_deployer::RestartPolicy::UnlessStopped,
            log_path: "/tmp/app.log".into(),
            command: Some(vec!["node".into(), "server.js".into()]),
            log_config: None,
            labels: HashMap::from([("sh.temps.deploy_id".into(), "42".into())]),
            project_slug: Some("shop".into()),
            control_plane_grants_socket: false,
        }
    }

    #[test]
    fn a_replica_becomes_a_pinned_long_running_deployment_workload() {
        let spec = deployment_workload_spec(
            &request(),
            42,
            CONTROL_PLANE_NODE_ID,
            "staged:k".into(),
            Isolation::Process,
        )
        .unwrap();
        assert_eq!(spec.owner, WorkloadRef::new(WorkloadKind::Deployment, 42));
        assert_eq!(spec.placement.nodes, Some(vec![0]), "placed by the job");
        let on_worker =
            deployment_workload_spec(&request(), 42, 7, "staged:k".into(), Isolation::Process)
                .unwrap();
        assert_eq!(
            on_worker.placement.nodes,
            Some(vec![7]),
            "the worker the job chose"
        );
        assert_eq!(
            spec.lifetime,
            Lifetime::LongRunning {
                restart: temps_workload::RestartPolicy::UnlessStopped
            }
        );
        assert!(spec.env.vars.is_empty(), "variables are never stored");
        assert_eq!(spec.env.imports, vec!["staged:k".to_string()]);
        assert_eq!(spec.ports[0].container_port, 3000);
        assert_eq!(
            spec.network.extra_networks,
            vec!["shop_default".to_string()]
        );
        let limits = spec.resources.limits.expect("cpu limit");
        assert_eq!(limits.cpu_millis, Some(500));
        assert_eq!(limits.memory_mb, None, "0 MB is uncapped");
        assert_eq!(
            spec.labels.get("sh.temps.deploy_id").map(String::as_str),
            Some("42")
        );
    }

    #[test]
    fn secret_files_and_the_socket_never_reach_the_stored_spec() {
        let mut with_both = request();
        with_both
            .secrets
            .insert("DB_PASSWORD".into(), "seeded-secret-value".into());
        with_both.control_plane_grants_socket = true;
        let spec = deployment_workload_spec(
            &with_both,
            42,
            CONTROL_PLANE_NODE_ID,
            "staged:k".into(),
            Isolation::Process,
        )
        .expect("built");
        let stored = serde_json::to_string(&spec).expect("serialised");
        assert!(!stored.contains("seeded-secret-value"), "{stored}");
        assert_eq!(spec.env.imports, vec!["staged:k".to_string()]);
        // Admission decides the profile from this host's own grant; the
        // request cannot ask for the socket itself.
        assert_eq!(spec.security, None);
    }

    #[test]
    fn a_micro_vm_replica_is_refused_what_the_vm_cannot_carry() {
        let micro_vm = |request: &DeployRequest| {
            deployment_workload_spec(
                request,
                42,
                CONTROL_PLANE_NODE_ID,
                "k".into(),
                Isolation::MicroVm,
            )
            .map(|spec| spec.isolation)
            .map_err(|error| error.to_string())
        };
        // The fixture joins a compose network: a microVM has no Docker
        // network to join.
        let message = micro_vm(&request()).unwrap_err();
        assert!(
            message.contains("shop_default") && message.contains("workload_isolation"),
            "{message}"
        );

        let mut plain = request();
        plain.extra_networks.clear();
        assert_eq!(micro_vm(&plain), Ok(Isolation::MicroVm));

        let mut with_secret = plain.clone();
        with_secret
            .secrets
            .insert("DATABASE_PASSWORD".into(), "x".into());
        let message = micro_vm(&with_secret).unwrap_err();
        assert!(message.contains("/run/secrets"), "{message}");
        assert!(
            !message.contains("DATABASE_PASSWORD"),
            "never names a secret"
        );

        let mut with_socket = plain;
        with_socket.control_plane_grants_socket = true;
        let message = micro_vm(&with_socket).unwrap_err();
        assert!(
            message.contains("ADR 045") && message.contains("'shop'"),
            "{message}"
        );
    }
}
