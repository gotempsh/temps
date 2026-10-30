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
//! (inspect, logs, exec, retire) instead of the local deployer. Only a
//! replica whose workload asks for `micro_vm` takes that path: a worker's
//! Docker replica runs behind a plane driver too (`node-stream`,
//! `agent-api`), but it is an ordinary container its node's deployer
//! inspects, execs into and follows the logs of.
//!
//! Retiring a deployment's replicas after promotion happens outside the
//! deploy job, so [`deployment_replica_workload`] and
//! [`retire_replica_workload`] let that cleanup find and delete the plane
//! workload behind a container: removing only the container would leave
//! the workload (and a microVM) running, or let the reconciler start it
//! again.

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

use super::DeploymentError;

/// Owner kind of a deployment's workloads.
pub const DEPLOYMENT_OWNER_KIND: &str = "deployment";

/// The owner every replica of deployment `deployment_id` is applied under.
pub fn deployment_owner(deployment_id: i32) -> Owner {
    Owner::new(DEPLOYMENT_OWNER_KIND, i64::from(deployment_id))
}

/// Whether a replica whose workload asks for `isolation` runs somewhere
/// its node's container deployer cannot reach (a microVM), so inspect,
/// exec and logs must go through the plane. Decided by what the workload
/// asked for, never by the driver's name: every worker replica runs behind
/// a non-`docker` driver and is still an ordinary container.
pub fn served_by_plane_only(isolation: Isolation) -> bool {
    isolation == Isolation::MicroVm
}

/// The plane workload behind one of a deployment's containers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplicaWorkload {
    pub workload_id: i64,
    /// The workload is already deleted: the plane is removing (or has
    /// removed) its instance.
    pub retired: bool,
}

/// Why a plane-started replica could not be retired.
#[derive(Debug, thiserror::Error)]
pub enum ReplicaRetireError {
    #[error(
        "Failed to look up the workload-plane workload of container {container_id} (deployment \
         {deployment_id}): {source}"
    )]
    Lookup {
        deployment_id: i32,
        container_id: String,
        #[source]
        source: sea_orm::DbErr,
    },

    #[error(
        "Container {container_id} of deployment {deployment_id} was started through the \
         workload plane as workload {workload_id}, but no workload plane runs in this process \
         to retire it; it is left in place for a later cleanup"
    )]
    PlaneUnavailable {
        deployment_id: i32,
        container_id: String,
        workload_id: i64,
    },

    #[error(
        "Workload plane could not retire workload {workload_id} (container {container_id}, \
         deployment {deployment_id}): {reason}"
    )]
    Delete {
        deployment_id: i32,
        container_id: String,
        workload_id: i64,
        reason: String,
    },
}

/// The workload deployment `deployment_id` applied for runtime container
/// `container_id`, when that replica was started through the plane
/// (`workload_mode = enforce`, on the control plane or a worker). `None`
/// for a legacy replica, which its node's deployer removes as before. One
/// query.
pub async fn deployment_replica_workload(
    db: &sea_orm::DatabaseConnection,
    deployment_id: i32,
    container_id: &str,
) -> Result<Option<ReplicaWorkload>, ReplicaRetireError> {
    use sea_orm::sea_query::Expr;
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect};
    use temps_entities::{workload_instances, workloads};

    let row: Option<(i64, Option<chrono::DateTime<chrono::Utc>>)> =
        workload_instances::Entity::find()
            .inner_join(workloads::Entity)
            .filter(Expr::cust_with_values(
                "\"workload_instances\".\"handle\"->>'id' = $1",
                [container_id],
            ))
            .filter(workloads::Column::OwnerKind.eq(DEPLOYMENT_OWNER_KIND))
            .filter(workloads::Column::OwnerId.eq(i64::from(deployment_id)))
            .order_by_desc(workload_instances::Column::Id)
            .select_only()
            .column(workloads::Column::Id)
            .column(workloads::Column::DeletedAt)
            .into_tuple()
            .one(db)
            .await
            .map_err(|source| ReplicaRetireError::Lookup {
                deployment_id,
                container_id: container_id.to_string(),
                source,
            })?;
    Ok(row.map(|(workload_id, deleted_at)| ReplicaWorkload {
        workload_id,
        retired: deleted_at.is_some(),
    }))
}

/// Retire `workload`, found by [`deployment_replica_workload`]: the plane
/// stops and removes its instance (container or microVM) and never starts
/// it again. A workload already retired needs no plane.
pub async fn retire_replica_workload(
    plane: Option<&WorkloadPlane>,
    deployment_id: i32,
    container_id: &str,
    workload: ReplicaWorkload,
) -> Result<(), ReplicaRetireError> {
    if workload.retired {
        return Ok(());
    }
    let plane = plane.ok_or_else(|| ReplicaRetireError::PlaneUnavailable {
        deployment_id,
        container_id: container_id.to_string(),
        workload_id: workload.workload_id,
    })?;
    plane
        .delete(
            workload.workload_id,
            "Retired",
            &format!(
                "deployment {deployment_id} was superseded or rejected; container \
                 {container_id} retired"
            ),
        )
        .await
        .map_err(|e| ReplicaRetireError::Delete {
            deployment_id,
            container_id: container_id.to_string(),
            workload_id: workload.workload_id,
            reason: e.to_string(),
        })
}

/// The control plane's microVM runtime (ADR-055 Phase 10), registered by
/// the agents plugin when this host has a working Firecracker. The
/// deployments plugin adds it to the workload plane; without it a
/// `micro_vm` replica is refused with the scheduler's reason.
pub struct ControlPlaneMicroVmDriver(pub Arc<dyn RuntimeDriver>);

/// How many log lines a microVM replica's log read returns.
const MICRO_VM_LOG_TAIL: u32 = 500;

/// How long a read remembers that an id is not a plane replica. A Docker
/// container is read by the health monitor every cycle; this keeps that from
/// costing a database lookup each time, while a replica the plane has only
/// just started is looked for again within seconds.
const NOT_A_REPLICA_TTL: Duration = Duration::from_secs(15);

/// A CPU sample older than this is a long enough window to compute a rate
/// from; a younger one is too noisy, so the reader takes a fresh pair.
const CPU_WINDOW_MIN: Duration = Duration::from_secs(1);

/// The gap between the two samples taken when there is no usable previous
/// one.
const CPU_SAMPLE_GAP: Duration = Duration::from_millis(500);

/// Most distinct replicas whose last CPU sample is remembered.
const CPU_SAMPLES_MAX: usize = 256;

/// CPU usage as a percentage where 100 is one full core, from CPU time used
/// over a wall-clock window. A counter that went backwards (the guest process
/// restarted and its counters began again) is not a negative rate: 0.
pub fn cpu_percent_between(previous_nanos: u64, current_nanos: u64, elapsed: Duration) -> f64 {
    let wall = elapsed.as_nanos();
    if wall == 0 {
        return 0.0;
    }
    let used = current_nanos.saturating_sub(previous_nanos);
    (used as f64 / wall as f64) * 100.0
}

#[derive(Clone, Copy)]
enum Classified {
    Replica(i64),
    NotReplica(std::time::Instant),
}

/// Reads microVM replicas through the workload plane, for
/// [`temps_deployer::replica_aware::ReplicaAwareDeployer`].
pub struct PlaneReplicaReader {
    plane: Arc<WorkloadPlane>,
    classified: Mutex<HashMap<String, Classified>>,
    /// Container id -> (when, cumulative CPU nanoseconds) of its last stats
    /// read, so the next read has a rate window without sleeping.
    cpu_samples: Mutex<HashMap<String, (std::time::Instant, u64)>>,
}

impl PlaneReplicaReader {
    pub fn new(plane: Arc<WorkloadPlane>) -> Self {
        Self {
            plane,
            classified: Mutex::new(HashMap::new()),
            cpu_samples: Mutex::new(HashMap::new()),
        }
    }

    async fn workload(&self, container_id: &str) -> Result<Option<i64>, DeployerError> {
        let cached = self
            .classified
            .lock()
            .ok()
            .and_then(|known| known.get(container_id).copied());
        match cached {
            Some(Classified::Replica(workload_id)) => return Ok(Some(workload_id)),
            Some(Classified::NotReplica(at)) if at.elapsed() < NOT_A_REPLICA_TTL => {
                return Ok(None)
            }
            _ => {}
        }
        let workload = micro_vm_replica_workload(&self.plane, container_id)
            .await
            .map_err(|e| DeployerError::DeploymentFailed(e.to_string()))?;
        if let Ok(mut known) = self.classified.lock() {
            if known.len() >= CPU_SAMPLES_MAX * 4 {
                known.retain(|_, entry| match entry {
                    Classified::Replica(_) => true,
                    Classified::NotReplica(at) => at.elapsed() < NOT_A_REPLICA_TTL,
                });
            }
            known.insert(
                container_id.to_string(),
                match workload {
                    Some(workload_id) => Classified::Replica(workload_id),
                    None => Classified::NotReplica(std::time::Instant::now()),
                },
            );
        }
        Ok(workload)
    }

    fn previous_cpu_sample(&self, container_id: &str) -> Option<(std::time::Instant, u64)> {
        self.cpu_samples
            .lock()
            .ok()
            .and_then(|samples| samples.get(container_id).copied())
            .filter(|(at, _)| at.elapsed() >= CPU_WINDOW_MIN)
    }

    fn remember_cpu_sample(&self, container_id: &str, at: std::time::Instant, nanos: u64) {
        if let Ok(mut samples) = self.cpu_samples.lock() {
            if samples.len() >= CPU_SAMPLES_MAX {
                samples.retain(|_, (taken, _)| taken.elapsed() < Duration::from_secs(600));
            }
            samples.insert(container_id.to_string(), (at, nanos));
        }
    }
}

fn plane_failure(
    container_id: &str,
    operation: &str,
    reason: impl std::fmt::Display,
) -> DeployerError {
    DeployerError::DeploymentFailed(format!(
        "Workload plane could not {operation} microVM replica {container_id}: {reason}"
    ))
}

#[async_trait]
impl temps_deployer::replica_aware::ReplicaReader for PlaneReplicaReader {
    async fn info(&self, container_id: &str) -> Result<Option<ContainerInfo>, DeployerError> {
        let Some(workload_id) = self.workload(container_id).await? else {
            return Ok(None);
        };
        let view = self
            .plane
            .get(workload_id)
            .await
            .map_err(|e| plane_failure(container_id, "read the workload of", e))?;
        let (handle, status) = self
            .plane
            .instance_status(workload_id)
            .await
            .map_err(|e| plane_failure(container_id, "inspect", e))?
            .ok_or_else(|| {
                plane_failure(container_id, "inspect", "its workload has no instance")
            })?;
        Ok(Some(micro_vm_container_info(&view.spec, &handle, &status)))
    }

    async fn stats(
        &self,
        container_id: &str,
    ) -> Result<Option<temps_deployer::ContainerStats>, DeployerError> {
        let Some(workload_id) = self.workload(container_id).await? else {
            return Ok(None);
        };
        let view = self
            .plane
            .get(workload_id)
            .await
            .map_err(|e| plane_failure(container_id, "read the workload of", e))?;
        let (handle, status) = self
            .plane
            .instance_status(workload_id)
            .await
            .map_err(|e| plane_failure(container_id, "inspect", e))?
            .ok_or_else(|| {
                plane_failure(container_id, "inspect", "its workload has no instance")
            })?;
        let read = || async {
            let at = std::time::Instant::now();
            self.plane
                .instance_stats(workload_id)
                .await
                .map_err(|e| plane_failure(container_id, "read the stats of", e))?
                .map(|stats| (at, stats))
                .ok_or_else(|| plane_failure(container_id, "read the stats of", "no instance"))
        };
        let (at, stats) = read().await?;
        let cpu_percent = match (stats.cpu_nanos, self.previous_cpu_sample(container_id)) {
            (Some(now), Some((before_at, before))) => {
                cpu_percent_between(before, now, at.duration_since(before_at))
            }
            (Some(first), None) => {
                // No usable earlier sample: take a second one a moment later.
                tokio::time::sleep(CPU_SAMPLE_GAP).await;
                let (later_at, later) = read().await?;
                match later.cpu_nanos {
                    Some(second) => cpu_percent_between(first, second, later_at.duration_since(at)),
                    None => 0.0,
                }
            }
            (None, _) => 0.0,
        };
        if let Some(nanos) = stats.cpu_nanos {
            self.remember_cpu_sample(container_id, at, nanos);
        }
        let memory_limit_bytes = stats.memory_limit_bytes;
        let memory_bytes = stats.memory_bytes.unwrap_or(0);
        let cpu_limit_cores = view
            .spec
            .resources
            .limits
            .as_ref()
            .and_then(|limits| limits.cpu_millis)
            .map(|millis| f64::from(millis) / 1000.0);
        Ok(Some(temps_deployer::ContainerStats {
            container_id: handle.id,
            container_name: handle.name,
            cpu_percent,
            cpu_limit_cores,
            online_cpus: None,
            memory_bytes,
            memory_limit_bytes,
            memory_percent: memory_limit_bytes
                .filter(|limit| *limit > 0)
                .map(|limit| ((memory_bytes as f64 / limit as f64) * 100.0).clamp(0.0, 100.0)),
            network_rx_bytes: stats.net_rx_bytes.unwrap_or(0),
            network_tx_bytes: stats.net_tx_bytes.unwrap_or(0),
            block_read_bytes: stats.disk_read_bytes,
            block_write_bytes: stats.disk_write_bytes,
            restart_count: None,
            started_at: status.started_at,
            timestamp: chrono::Utc::now(),
        }))
    }

    async fn logs(&self, container_id: &str) -> Result<Option<Vec<String>>, DeployerError> {
        let Some(workload_id) = self.workload(container_id).await? else {
            return Ok(None);
        };
        let lines = self
            .plane
            .instance_logs(workload_id, MICRO_VM_LOG_TAIL)
            .await
            .map_err(|e| plane_failure(container_id, "read the logs of", e))?
            .unwrap_or_default();
        Ok(Some(
            lines
                .iter()
                .map(|line| String::from_utf8_lossy(&line.line).into_owned())
                .collect(),
        ))
    }
}

/// The workload behind `container_id` when it is a microVM replica, which
/// no container runtime on the host can see. `None` for anything else: a
/// container the local Docker serves, or an id the plane never started.
pub async fn micro_vm_replica_workload(
    plane: &WorkloadPlane,
    container_id: &str,
) -> Result<Option<i64>, DeploymentError> {
    let plane_error =
        |operation: &'static str, reason: String| DeploymentError::ContainerOperation {
            container_id: container_id.to_string(),
            operation,
            location: "workload plane".to_string(),
            reason,
        };
    let Some((workload_id, _driver)) = plane
        .workload_for_instance(container_id)
        .await
        .map_err(|e| plane_error("look up the workload", e.to_string()))?
    else {
        return Ok(None);
    };
    match plane.get(workload_id).await {
        Ok(view) => Ok(served_by_plane_only(view.spec.isolation).then_some(workload_id)),
        // Gone with its instances: nothing the plane could serve.
        Err(temps_workload_plane::PlaneError::NotFound { .. }) => Ok(None),
        Err(e) => Err(plane_error("read the workload", e.to_string())),
    }
}

/// The current log tail of a microVM replica (a microVM's logs are read,
/// not followed), or `None` when `container_id` is not one.
pub async fn micro_vm_replica_logs(
    plane: &WorkloadPlane,
    container_id: &str,
) -> Result<Option<Vec<String>>, DeploymentError> {
    let Some(workload_id) = micro_vm_replica_workload(plane, container_id).await? else {
        return Ok(None);
    };
    let lines = plane
        .instance_logs(workload_id, MICRO_VM_LOG_TAIL)
        .await
        .map_err(|e| DeploymentError::ContainerOperation {
            container_id: container_id.to_string(),
            operation: "read the logs",
            location: "workload plane".to_string(),
            reason: e.to_string(),
        })?
        .unwrap_or_default();
    Ok(Some(
        lines
            .iter()
            .map(|line| String::from_utf8_lossy(&line.line).into_owned())
            .collect(),
    ))
}

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
        disk_io_mbps: None,
        network_mbps: None,
    };
    Ok(WorkloadSpec {
        owner: WorkloadRef::new(WorkloadKind::Deployment, i64::from(deployment_id)),
        artifact: Artifact::oci(request.image_name.clone()),
        isolation,
        process: Process {
            command: request.command.clone(),
            entrypoint: request.entrypoint.clone(),
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
    /// Container id -> its workload when only the plane can serve it (a
    /// microVM), `None` when its node's deployer does. A replica's
    /// isolation never changes, so the health check's polling asks once.
    plane_only: Mutex<HashMap<String, Option<i64>>>,
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
            plane_only: Mutex::new(HashMap::new()),
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

    /// The plane workload behind `container_id` when it runs in a microVM,
    /// which no container deployer can reach, so the caller must go through
    /// the plane. `None` for a container -- on the control plane or a
    /// worker, whatever driver the plane started it with: the node's
    /// deployer serves it as before.
    async fn plane_only_workload(&self, container_id: &str) -> Result<Option<i64>, DeployerError> {
        if let Some(known) = self
            .plane_only
            .lock()
            .ok()
            .and_then(|known| known.get(container_id).copied())
        {
            return Ok(known);
        }
        let found = self
            .plane
            .workload_for_instance(container_id)
            .await
            .map_err(|e| self.plane_error(&format!("look up instance {container_id}"), e))?;
        let Some((workload_id, driver)) = found else {
            // Not (or no longer) a plane instance. Not remembered, so a
            // later call looks again.
            return Ok(None);
        };
        let isolation = match self.plane.get(workload_id).await {
            Ok(view) => view.spec.isolation,
            // Gone with its instances: nothing the plane could serve.
            Err(temps_workload_plane::PlaneError::NotFound { .. }) => return Ok(None),
            Err(e) => {
                return Err(self.plane_error(
                    &format!("read workload {workload_id} of instance {container_id}"),
                    e,
                ))
            }
        };
        let plane_only = served_by_plane_only(isolation).then_some(workload_id);
        tracing::debug!(
            deployment_id = self.deployment_id,
            container_id,
            workload_id,
            driver = %driver,
            plane_only = plane_only.is_some(),
            "classified a plane replica"
        );
        if let Ok(mut known) = self.plane_only.lock() {
            known.insert(container_id.to_string(), plane_only);
        }
        Ok(plane_only)
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
            owner: deployment_owner(self.deployment_id),
            name: request.container_name.clone(),
            spec,
            project_slug: request.project_slug.clone(),
            finalizers: Vec::new(),
        };
        let start = |e: temps_workload_plane::PlaneError| {
            self.plane_error(&format!("start {}", request.container_name), e)
        };
        let workload_id = match self.volume_owner.as_ref() {
            None => self.plane.apply(apply).await.map_err(start)?,
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
                applied.workload_id
            }
        };
        // Until the replica is handed back, a failed, timed-out or cancelled
        // wait retires the workload: the reconciler may still be creating
        // its container, which nothing would otherwise track or remove.
        let guard = self.plane.retire_on_drop(
            workload_id,
            "HandoffFailed",
            format!(
                "deployment {} stopped waiting for container {}",
                self.deployment_id, request.container_name
            ),
        );
        let handed_over = self
            .await_replica(workload_id, &request.container_name, container_port)
            .await;
        // The values were only needed for the create, which is done: on a
        // worker, its node fetched them before it reported the replica
        // running, and `await_replica` waits for that report.
        drop(staged);
        let (view, handle, status) = match handed_over {
            Ok(replica) => {
                guard.disarm();
                replica
            }
            Err(error) => {
                if let Err(retire_error) = guard.retire(&error.to_string()).await {
                    tracing::error!(
                        deployment_id = self.deployment_id,
                        workload_id,
                        "retiring workload {workload_id} after its failed handoff failed: \
                         {retire_error}"
                    );
                }
                return Err(error);
            }
        };
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

/// How often a handoff rereads a worker's replica while it waits for the
/// node's report of its published port.
const REPORT_POLL: Duration = Duration::from_millis(500);

impl PlaneContainerDeployer {
    /// Wait for workload `workload_id`'s replica to run, then read its
    /// instance: everything between the apply and the handoff that can fail.
    ///
    /// The handoff needs what the node itself reported: the instance
    /// running and, on a worker, the host port it published for
    /// `container_port` (what the control-plane proxy dials). A worker's
    /// node fetches the staged environment when it creates the container,
    /// so the caller may drop it only after this returns.
    async fn await_replica(
        &self,
        workload_id: i64,
        container_name: &str,
        container_port: u16,
    ) -> Result<
        (
            temps_workload_plane::WorkloadView,
            temps_workload::InstanceHandle,
            temps_workload::InstanceStatus,
        ),
        DeployerError,
    > {
        let started = tokio::time::Instant::now();
        let view = self
            .plane
            .wait_for(workload_id, ConditionKind::Running, self.ready_timeout)
            .await
            .map_err(|e| {
                self.plane_error(
                    &format!("start {container_name} (workload {workload_id})"),
                    e,
                )
            })?;
        // On the control plane the runtime answers directly, as before.
        let on_worker = self.node_id != CONTROL_PLANE_NODE_ID;
        let needs_port = on_worker && container_port != 0;
        loop {
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
            let published = status.published_ports.iter().any(|binding| {
                binding.container_port == container_port && binding.host_port.is_some()
            });
            if !on_worker
                || (status.state == temps_workload::InstanceState::Running
                    && (published || !needs_port))
            {
                return Ok((view, handle, status));
            }
            if started.elapsed() >= self.ready_timeout {
                return Err(self.plane_error(
                    &format!("hand over {container_name} (workload {workload_id})"),
                    format!(
                        "after {}s its instance '{}' on node {} is {:?}{}",
                        self.ready_timeout.as_secs(),
                        handle.name,
                        self.node_id,
                        status.state,
                        if needs_port && !published {
                            format!(
                                " with no host port published for container port {container_port}"
                            )
                        } else {
                            String::new()
                        }
                    ),
                ));
            }
            tokio::time::sleep(REPORT_POLL).await;
        }
    }

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
    use temps_deployer::replica_aware::ReplicaReader;
    use temps_workload as tw;

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
            entrypoint: None,
            log_config: None,
            labels: HashMap::from([("sh.temps.deploy_id".into(), "42".into())]),
            project_slug: Some("shop".into()),
            control_plane_grants_socket: false,
        }
    }

    #[test]
    fn dockerfile_runtime_overrides_survive_workload_translation() {
        for entrypoint in [None, Some(vec![]), Some(vec!["/app/start".into()])] {
            let mut request = request();
            request.entrypoint = entrypoint.clone();
            let spec = deployment_workload_spec(
                &request,
                42,
                CONTROL_PLANE_NODE_ID,
                "staged:k".into(),
                Isolation::Process,
            )
            .unwrap();
            assert_eq!(spec.process.command, request.command);
            assert_eq!(spec.process.entrypoint, entrypoint);
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

    /// The node's deployer, which a handoff that never completes never calls.
    struct UnusedDeployer;

    fn unused(operation: &str) -> DeployerError {
        DeployerError::DeploymentFailed(format!("the test deployer does not {operation}"))
    }

    #[async_trait]
    impl ContainerDeployer for UnusedDeployer {
        async fn deploy_container(&self, _: DeployRequest) -> Result<DeployResult, DeployerError> {
            Err(unused("deploy"))
        }
        async fn exec_in_container(
            &self,
            _: &str,
            _: Vec<String>,
            _: Duration,
        ) -> Result<temps_deployer::ExecResult, DeployerError> {
            Err(unused("exec"))
        }
        async fn start_container(&self, _: &str) -> Result<(), DeployerError> {
            Err(unused("start"))
        }
        async fn stop_container(&self, _: &str) -> Result<(), DeployerError> {
            Err(unused("stop"))
        }
        async fn pause_container(&self, _: &str) -> Result<(), DeployerError> {
            Err(unused("pause"))
        }
        async fn resume_container(&self, _: &str) -> Result<(), DeployerError> {
            Err(unused("resume"))
        }
        async fn remove_container(&self, _: &str) -> Result<(), DeployerError> {
            Err(unused("remove"))
        }
        async fn get_container_info(&self, _: &str) -> Result<ContainerInfo, DeployerError> {
            Err(unused("inspect"))
        }
        async fn get_container_stats(
            &self,
            _: &str,
        ) -> Result<temps_deployer::ContainerStats, DeployerError> {
            Err(unused("read stats"))
        }
        async fn list_containers(&self) -> Result<Vec<ContainerInfo>, DeployerError> {
            Ok(Vec::new())
        }
        async fn get_container_logs(&self, _: &str) -> Result<String, DeployerError> {
            Err(unused("read logs"))
        }
        async fn stream_container_logs(
            &self,
            _: &str,
        ) -> Result<Box<dyn futures::Stream<Item = String> + Unpin + Send>, DeployerError> {
            Err(unused("stream logs"))
        }
    }

    struct AcceptingAudit;

    #[async_trait]
    impl temps_core::AuditLogger for AcceptingAudit {
        async fn create_audit_log(
            &self,
            _operation: &dyn temps_core::AuditOperation,
        ) -> temps_core::anyhow::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn a_handoff_that_times_out_retires_the_applied_workload() {
        use sea_orm::EntityTrait;
        let Ok(test_db) = temps_database::test_utils::TestDatabase::with_migrations().await else {
            println!("Docker not available, skipping");
            return;
        };
        let db = test_db.connection_arc();
        // No reconciler runs, so the replica never reports Running.
        let plane = Arc::new(WorkloadPlane::new(
            db.clone(),
            temps_workload_plane::NodeDriver(None),
            Arc::new(temps_workload_plane::NoEnvSource),
            Arc::new(AcceptingAudit),
            temps_workload_plane::PlaneConfig::new("adr055-handoff-test"),
        ));
        let deployer = PlaneContainerDeployer::new(
            plane,
            Arc::new(UnusedDeployer),
            42,
            Duration::from_millis(300),
        );
        let error = deployer
            .deploy_container(request())
            .await
            .expect_err("the wait times out");
        assert!(error.to_string().contains("app-production-42-1"), "{error}");
        let rows = temps_entities::workloads::Entity::find()
            .all(db.as_ref())
            .await
            .expect("workloads");
        assert_eq!(rows.len(), 1, "one workload was applied");
        assert!(
            rows[0].deleted_at.is_some(),
            "and retired, so the reconciler removes whatever it created"
        );
    }

    /// Places every attempt on a worker that reads the node state stream.
    struct StreamWorker {
        db: Arc<sea_orm::DatabaseConnection>,
    }

    #[async_trait]
    impl temps_workload_plane::AttemptPlacement for StreamWorker {
        async fn place(
            &self,
            _: &temps_workload_plane::WorkloadView,
        ) -> Result<temps_workload_plane::Placed, String> {
            Err("replicas are pinned by the deploy job".into())
        }

        async fn place_on(
            &self,
            _: &temps_workload_plane::WorkloadView,
            node_id: i32,
        ) -> Result<temps_workload_plane::Placed, String> {
            Ok(temps_workload_plane::Placed::Node {
                node_id,
                node_name: format!("worker-{node_id}"),
                driver: self
                    .driver_for(node_id, temps_workload_plane::STREAM_DRIVER)
                    .await?,
            })
        }

        async fn driver_for(
            &self,
            node_id: i32,
            _: &str,
        ) -> Result<Arc<dyn temps_workload::RuntimeDriver>, String> {
            // Nothing listens at the agent's address: the image is taken as
            // already on the node, and the handoff never reads output.
            let remote = temps_deployer::remote::RemoteNodeDeployer::new(
                "http://127.0.0.1:9".into(),
                "unused-token".into(),
                format!("worker-{node_id}"),
            )
            .map_err(|e| e.to_string())?;
            let rpc = ImageOnNode(crate::services::node_state::AgentApiDriver::new(
                node_id,
                format!("worker-{node_id}"),
                Arc::new(remote),
                None,
            ));
            Ok(Arc::new(
                temps_workload_plane::StreamNodeDriver::new(
                    self.db.clone(),
                    node_id,
                    Arc::new(rpc),
                )
                .with_poll_interval(Duration::from_millis(100)),
            ))
        }
    }

    /// A worker's agent API whose image is already on the node.
    struct ImageOnNode(crate::services::node_state::AgentApiDriver);

    #[async_trait]
    impl tw::RuntimeDriver for ImageOnNode {
        fn capabilities(&self) -> &tw::DriverCapabilities {
            self.0.capabilities()
        }
        async fn prepare(
            &self,
            artifact: &tw::Artifact,
        ) -> Result<tw::PreparedArtifact, tw::DriverError> {
            Ok(tw::PreparedArtifact {
                artifact: artifact.clone(),
                local_ref: "image-on-node".into(),
            })
        }
        async fn create(
            &self,
            spec: &tw::InstanceSpec,
            attach: &tw::Attachments,
        ) -> Result<tw::InstanceHandle, tw::DriverError> {
            self.0.create(spec, attach).await
        }
        async fn start(&self, handle: &tw::InstanceHandle) -> Result<(), tw::DriverError> {
            self.0.start(handle).await
        }
        async fn stop(&self, handle: &tw::InstanceHandle) -> Result<(), tw::DriverError> {
            self.0.stop(handle).await
        }
        async fn remove(
            &self,
            handle: &tw::InstanceHandle,
            purge: bool,
        ) -> Result<(), tw::DriverError> {
            self.0.remove(handle, purge).await
        }
        async fn inspect(
            &self,
            handle: &tw::InstanceHandle,
        ) -> Result<tw::InstanceStatus, tw::DriverError> {
            self.0.inspect(handle).await
        }
        async fn logs(
            &self,
            handle: &tw::InstanceHandle,
            query: tw::LogQuery,
        ) -> Result<tw::LogStream, tw::DriverError> {
            self.0.logs(handle, query).await
        }
        async fn stats(
            &self,
            handle: &tw::InstanceHandle,
        ) -> Result<tw::InstanceStats, tw::DriverError> {
            self.0.stats(handle).await
        }
    }

    fn report(instance_id: i64, host_port: Option<u16>) -> temps_workload::stream::InstanceReport {
        temps_workload::stream::InstanceReport {
            instance_id,
            phase: "running".into(),
            exit_code: None,
            started_at: Some(chrono::Utc::now()),
            finished_at: None,
            conditions: Vec::new(),
            published_ports: host_port
                .map(|port| temps_workload::PortBinding {
                    container_port: 3000,
                    protocol: temps_workload::PortProtocol::Tcp,
                    host_ip: "10.0.0.7".into(),
                    host_port: Some(port),
                })
                .into_iter()
                .collect(),
        }
    }

    /// A replica on a stream worker is handed over only once its node
    /// reported it running with the deployment's port published, and the
    /// staged environment the node creates it from outlives that wait.
    #[tokio::test]
    async fn a_worker_handoff_waits_for_the_node_report_and_its_published_port() {
        use sea_orm::EntityTrait;
        let Ok(test_db) = temps_database::test_utils::TestDatabase::with_migrations().await else {
            println!("Docker not available, skipping");
            return;
        };
        let db = test_db.connection_arc();
        let plane = Arc::new(
            WorkloadPlane::new(
                db.clone(),
                temps_workload_plane::NodeDriver(None),
                Arc::new(temps_workload_plane::NoEnvSource),
                Arc::new(AcceptingAudit),
                temps_workload_plane::PlaneConfig::new("adr055-worker-handoff"),
            )
            .with_placement(Arc::new(StreamWorker { db: db.clone() })),
        );
        let deployer = Arc::new(
            PlaneContainerDeployer::new(
                plane.clone(),
                Arc::new(UnusedDeployer),
                42,
                Duration::from_secs(30),
            )
            .on_node(7),
        );
        let handoff = tokio::spawn({
            let deployer = deployer.clone();
            async move { deployer.deploy_container(request()).await }
        });

        let mut instance = None;
        for _ in 0..200 {
            plane.reconcile_at(chrono::Utc::now(), false).await;
            instance = temps_entities::workload_instances::Entity::find()
                .one(db.as_ref())
                .await
                .expect("read the instances");
            if instance.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let instance = instance.expect("the replica was assigned to the worker");
        assert_eq!(instance.node_id, Some(7));
        let workload = temps_entities::workloads::Entity::find()
            .one(db.as_ref())
            .await
            .expect("read the workload");
        assert_eq!(
            instance.phase, "pending",
            "nothing has run it yet: {instance:?} of {workload:?}"
        );

        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(!handoff.is_finished(), "no report, no handoff");
        let env = plane
            .instance_env(7, instance.id)
            .await
            .expect("the node can still fetch the staged environment");
        assert!(env.env.contains(&("PORT".into(), "3000".into())));

        // Running, but the port the proxy dials is not published yet.
        plane
            .ingest_node_reports(7, &[report(instance.id, None)])
            .await
            .expect("ingested");
        plane.reconcile_at(chrono::Utc::now(), false).await;
        tokio::time::sleep(REPORT_POLL * 3).await;
        assert!(
            !handoff.is_finished(),
            "running without its published port is not handed over"
        );

        plane
            .ingest_node_reports(7, &[report(instance.id, Some(40000))])
            .await
            .expect("ingested");
        plane.reconcile_at(chrono::Utc::now(), false).await;
        let result = tokio::time::timeout(Duration::from_secs(10), handoff)
            .await
            .expect("handed over after the report")
            .expect("joined")
            .expect("deployed");
        assert_eq!(result.container_port, 3000);
        assert_eq!(result.host_port, 40000);
        assert!(
            plane.instance_env(7, instance.id).await.is_err(),
            "the staged environment is dropped after the handoff"
        );
    }

    /// The node's deployer: answers exec and inspect itself, so a test sees
    /// which path served a replica.
    struct LocalDeployer;

    #[async_trait]
    impl ContainerDeployer for LocalDeployer {
        async fn deploy_container(&self, r: DeployRequest) -> Result<DeployResult, DeployerError> {
            UnusedDeployer.deploy_container(r).await
        }
        async fn exec_in_container(
            &self,
            container_id: &str,
            _: Vec<String>,
            _: Duration,
        ) -> Result<temps_deployer::ExecResult, DeployerError> {
            Ok(temps_deployer::ExecResult {
                exit_code: 0,
                output: format!("local exec in {container_id}"),
                truncated: false,
            })
        }
        async fn start_container(&self, id: &str) -> Result<(), DeployerError> {
            UnusedDeployer.start_container(id).await
        }
        async fn stop_container(&self, id: &str) -> Result<(), DeployerError> {
            UnusedDeployer.stop_container(id).await
        }
        async fn pause_container(&self, id: &str) -> Result<(), DeployerError> {
            UnusedDeployer.pause_container(id).await
        }
        async fn resume_container(&self, id: &str) -> Result<(), DeployerError> {
            UnusedDeployer.resume_container(id).await
        }
        async fn remove_container(&self, id: &str) -> Result<(), DeployerError> {
            UnusedDeployer.remove_container(id).await
        }
        async fn get_container_info(&self, id: &str) -> Result<ContainerInfo, DeployerError> {
            Err(DeployerError::DeploymentFailed(format!(
                "local inspect of {id}"
            )))
        }
        async fn get_container_stats(
            &self,
            id: &str,
        ) -> Result<temps_deployer::ContainerStats, DeployerError> {
            UnusedDeployer.get_container_stats(id).await
        }
        async fn list_containers(&self) -> Result<Vec<ContainerInfo>, DeployerError> {
            Ok(Vec::new())
        }
        async fn get_container_logs(&self, id: &str) -> Result<String, DeployerError> {
            UnusedDeployer.get_container_logs(id).await
        }
        async fn stream_container_logs(
            &self,
            id: &str,
        ) -> Result<Box<dyn futures::Stream<Item = String> + Unpin + Send>, DeployerError> {
            UnusedDeployer.stream_container_logs(id).await
        }
    }

    /// Apply deployment 42's replica `name` with `isolation`, and record the
    /// instance `driver` started for it as runtime id `container_id`.
    async fn plane_replica(
        plane: &WorkloadPlane,
        db: &sea_orm::DatabaseConnection,
        name: &str,
        isolation: Isolation,
        driver: &str,
        container_id: &str,
    ) -> i64 {
        use sea_orm::{ActiveModelTrait, Set};
        let mut plain = request();
        plain.extra_networks.clear();
        plain.container_name = name.to_string();
        let spec = deployment_workload_spec(
            &plain,
            42,
            CONTROL_PLANE_NODE_ID,
            "staged:k".into(),
            isolation,
        )
        .expect("spec");
        let workload_id = plane
            .apply(ApplyRequest {
                owner: deployment_owner(42),
                name: name.to_string(),
                spec,
                project_slug: Some("shop".into()),
                finalizers: Vec::new(),
            })
            .await
            .expect("applied");
        let now = chrono::Utc::now();
        temps_entities::workload_instances::ActiveModel {
            workload_id: Set(workload_id),
            revision: Set(1),
            attempt: Set(1),
            node_id: Set(None),
            driver: Set(driver.to_string()),
            handle: Set(serde_json::json!({
                "driver": driver,
                "id": container_id,
                "name": name,
            })),
            address: Set(None),
            phase: Set("running".into()),
            ready: Set(true),
            conditions: Set(serde_json::json!([])),
            exit_code: Set(None),
            published_ports: Set(serde_json::json!([])),
            started_at: Set(Some(now)),
            finished_at: Set(None),
            change_seq: Set(0),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("instance");
        workload_id
    }

    #[tokio::test]
    async fn worker_docker_replicas_are_served_by_the_node_deployer() {
        let Ok(test_db) = temps_database::test_utils::TestDatabase::with_migrations().await else {
            println!("Docker not available, skipping");
            return;
        };
        let db = test_db.connection_arc();
        let plane = Arc::new(WorkloadPlane::new(
            db.clone(),
            temps_workload_plane::NodeDriver(None),
            Arc::new(temps_workload_plane::NoEnvSource),
            Arc::new(AcceptingAudit),
            temps_workload_plane::PlaneConfig::new("adr055-classify-test"),
        ));
        // Worker Docker replicas, started by the node-stream and agent-api
        // drivers: ordinary containers behind the node's deployer.
        for (name, driver, container_id) in [
            ("app-production-42-1", "node-stream", "c-node-stream"),
            ("app-production-42-2", "agent-api", "c-agent-api"),
        ] {
            plane_replica(
                &plane,
                db.as_ref(),
                name,
                Isolation::Process,
                driver,
                container_id,
            )
            .await;
        }
        let micro_vm = plane_replica(
            &plane,
            db.as_ref(),
            "app-production-42-3",
            Isolation::MicroVm,
            "firecracker",
            "vm-42-3",
        )
        .await;

        let deployer = PlaneContainerDeployer::new(
            plane.clone(),
            Arc::new(LocalDeployer),
            42,
            Duration::from_secs(5),
        );
        for container_id in ["c-node-stream", "c-agent-api"] {
            assert_eq!(
                deployer
                    .plane_only_workload(container_id)
                    .await
                    .expect("classified"),
                None,
                "{container_id} is a container"
            );
            let exec = deployer
                .exec_in_container(container_id, vec!["true".into()], Duration::from_secs(5))
                .await
                .expect("exec");
            assert_eq!(exec.output, format!("local exec in {container_id}"));
            let inspect = deployer
                .get_container_info(container_id)
                .await
                .expect_err("the local test deployer refuses to inspect");
            assert!(
                inspect
                    .to_string()
                    .contains(&format!("local inspect of {container_id}")),
                "{inspect}"
            );
        }
        // A microVM is the plane's alone.
        assert_eq!(
            deployer
                .plane_only_workload("vm-42-3")
                .await
                .expect("classified"),
            Some(micro_vm)
        );
        let exec = deployer
            .exec_in_container("vm-42-3", vec!["true".into()], Duration::from_secs(5))
            .await;
        assert!(
            exec.map_or(true, |result| !result.output.starts_with("local")),
            "a microVM exec never reaches the node's deployer"
        );
        // A container the plane never started is the node deployer's too.
        assert_eq!(
            deployer
                .plane_only_workload("legacy-1")
                .await
                .expect("classified"),
            None
        );
    }

    /// A Firecracker stand-in: reports one running instance and a fixed log.
    struct FakeMicroVm {
        capabilities: tw::DriverCapabilities,
        /// The guest cannot be read: its log read fails.
        logs_fail: bool,
        stats_calls: std::sync::atomic::AtomicU64,
    }

    impl FakeMicroVm {
        fn new() -> Self {
            Self::with_logs_failing(false)
        }

        fn with_logs_failing(logs_fail: bool) -> Self {
            Self {
                logs_fail,
                stats_calls: std::sync::atomic::AtomicU64::new(0),
                capabilities: tw::DriverCapabilities {
                    name: "firecracker".into(),
                    artifacts: tw::ArtifactKinds::default(),
                    isolation: Isolation::MicroVm,
                    network_attachment: tw::AttachmentKind::Tap,
                    volumes: tw::VolumeKinds::default(),
                    exec: false,
                    snapshot: false,
                    resize_disk: false,
                    limits: tw::LimitSupport::default(),
                },
            }
        }
    }

    #[async_trait]
    impl tw::RuntimeDriver for FakeMicroVm {
        fn capabilities(&self) -> &tw::DriverCapabilities {
            &self.capabilities
        }
        async fn prepare(&self, _: &tw::Artifact) -> Result<tw::PreparedArtifact, tw::DriverError> {
            unreachable!("the read path never prepares an artifact")
        }
        async fn create(
            &self,
            _: &tw::InstanceSpec,
            _: &tw::Attachments,
        ) -> Result<tw::InstanceHandle, tw::DriverError> {
            unreachable!("the read path never creates an instance")
        }
        async fn start(&self, _: &tw::InstanceHandle) -> Result<(), tw::DriverError> {
            Ok(())
        }
        async fn stop(&self, _: &tw::InstanceHandle) -> Result<(), tw::DriverError> {
            Ok(())
        }
        async fn remove(&self, _: &tw::InstanceHandle, _: bool) -> Result<(), tw::DriverError> {
            Ok(())
        }
        async fn inspect(
            &self,
            _: &tw::InstanceHandle,
        ) -> Result<tw::InstanceStatus, tw::DriverError> {
            Ok(tw::InstanceStatus {
                state: tw::InstanceState::Running,
                exit_code: None,
                started_at: Some(chrono::Utc::now()),
                finished_at: None,
                address: Some("172.20.0.10".into()),
                published_ports: Vec::new(),
            })
        }
        async fn logs(
            &self,
            _: &tw::InstanceHandle,
            _: tw::LogQuery,
        ) -> Result<tw::LogStream, tw::DriverError> {
            use futures::StreamExt;
            if self.logs_fail {
                return Err(tw::DriverError::Unavailable {
                    driver: "firecracker".into(),
                    reason: "the guest agent does not answer".into(),
                });
            }
            let lines = ["Ready in 1440ms\n", "GET / 200\n"].map(|line| {
                Ok(tw::LogLine {
                    stderr: false,
                    line: bytes::Bytes::from(line),
                })
            });
            Ok(futures::stream::iter(lines).boxed())
        }
        async fn stats(
            &self,
            _: &tw::InstanceHandle,
        ) -> Result<tw::InstanceStats, tw::DriverError> {
            // CPU time advances 250 ms of a core's time per call.
            let calls = self
                .stats_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                + 1;
            Ok(tw::InstanceStats {
                cpu_nanos: Some(calls * 250_000_000),
                memory_bytes: Some(64 * 1024 * 1024),
                pids: Some(3),
                disk_read_bytes: Some(4096),
                disk_write_bytes: Some(8192),
                net_rx_bytes: Some(1000),
                net_tx_bytes: Some(2000),
                memory_limit_bytes: Some(256 * 1024 * 1024),
            })
        }
    }

    #[tokio::test]
    async fn the_containers_view_reads_a_micro_vm_replica_from_the_plane() {
        let Ok(test_db) = temps_database::test_utils::TestDatabase::with_migrations().await else {
            println!("Docker not available, skipping");
            return;
        };
        let db = test_db.connection_arc();
        let plane = Arc::new(
            WorkloadPlane::new(
                db.clone(),
                temps_workload_plane::NodeDriver(None),
                Arc::new(temps_workload_plane::NoEnvSource),
                Arc::new(AcceptingAudit),
                temps_workload_plane::PlaneConfig::new("adr055-read-test"),
            )
            .with_local_driver(Arc::new(FakeMicroVm::new())),
        );
        plane_replica(
            &plane,
            db.as_ref(),
            "app-production-42-1",
            Isolation::Process,
            "docker",
            "c-docker",
        )
        .await;
        let micro_vm = plane_replica(
            &plane,
            db.as_ref(),
            "app-production-42-2",
            Isolation::MicroVm,
            "firecracker",
            "vm-42-2",
        )
        .await;

        assert_eq!(
            micro_vm_replica_workload(&plane, "vm-42-2")
                .await
                .expect("classified"),
            Some(micro_vm)
        );

        let reader = PlaneReplicaReader::new(plane.clone());
        let info = reader
            .info("vm-42-2")
            .await
            .expect("inspected")
            .expect("a microVM replica is described by the plane");
        assert_eq!(info.container_id, "vm-42-2");
        assert_eq!(info.status, ContainerStatus::Running);

        // The log tail is what the guest wrote, one entry per line.
        let logs = reader
            .logs("vm-42-2")
            .await
            .expect("logs read")
            .expect("a microVM replica's logs come from the plane");
        assert_eq!(logs, vec!["Ready in 1440ms\n", "GET / 200\n"]);

        // Stats: the first read has no earlier sample, so it takes two a
        // moment apart; the fake driver burns 250 ms of CPU between them.
        let stats = reader
            .stats("vm-42-2")
            .await
            .expect("stats read")
            .expect("a microVM replica's stats come from the plane");
        assert_eq!(stats.memory_bytes, 64 * 1024 * 1024);
        assert_eq!(stats.memory_limit_bytes, Some(256 * 1024 * 1024));
        assert_eq!(stats.memory_percent, Some(25.0));
        assert_eq!(stats.network_rx_bytes, 1000);
        assert_eq!(stats.network_tx_bytes, 2000);
        assert_eq!(stats.block_read_bytes, Some(4096));
        assert_eq!(stats.block_write_bytes, Some(8192));
        // 250 ms of CPU over a ~500 ms gap: about half a core, never zero
        // and never more than a core for one busy thread.
        assert!(
            stats.cpu_percent > 5.0 && stats.cpu_percent <= 100.0,
            "{}",
            stats.cpu_percent
        );

        // Docker serves its own containers and ids the plane never started:
        // the plane answers "not mine" so the caller asks Docker.
        for id in ["c-docker", "legacy-1"] {
            assert_eq!(
                micro_vm_replica_workload(&plane, id)
                    .await
                    .expect("classified"),
                None,
                "{id} is not a microVM"
            );
            assert!(reader.info(id).await.expect("not an error").is_none());
            assert!(reader.stats(id).await.expect("not an error").is_none());
            assert!(reader.logs(id).await.expect("not an error").is_none());
        }
    }

    #[test]
    fn cpu_percent_is_the_share_of_one_core_over_the_window() {
        let second = Duration::from_secs(1);
        assert_eq!(cpu_percent_between(0, 500_000_000, second), 50.0);
        assert_eq!(cpu_percent_between(0, 2_000_000_000, second), 200.0);
        // A restarted process starts its counters again: not a negative rate.
        assert_eq!(cpu_percent_between(5_000_000_000, 1_000, second), 0.0);
        assert_eq!(cpu_percent_between(0, 100, Duration::ZERO), 0.0);
    }

    /// The logs view asks Docker for a container's logs. A microVM is not a
    /// Docker container, so its tail has to come from the plane, and every
    /// other id must still reach Docker.
    #[tokio::test]
    async fn a_micro_vms_logs_are_served_by_the_plane_not_docker() {
        use crate::services::container_operations::{
            ContainerOperations, LocalContainerOperations,
        };
        use futures::StreamExt;
        let Ok(test_db) = temps_database::test_utils::TestDatabase::with_migrations().await else {
            println!("Docker not available, skipping");
            return;
        };
        let db = test_db.connection_arc();
        let plane = Arc::new(
            WorkloadPlane::new(
                db.clone(),
                temps_workload_plane::NodeDriver(None),
                Arc::new(temps_workload_plane::NoEnvSource),
                Arc::new(AcceptingAudit),
                temps_workload_plane::PlaneConfig::new("adr055-log-test"),
            )
            .with_local_driver(Arc::new(FakeMicroVm::new())),
        );
        plane_replica(
            &plane,
            db.as_ref(),
            "app-production-42-1",
            Isolation::MicroVm,
            "firecracker",
            "vm-42-1",
        )
        .await;

        // No daemon behind either handle (the client only connects when
        // used): reaching Docker would fail.
        let Ok(docker) = bollard::Docker::connect_with_http(
            "http://127.0.0.1:1",
            5,
            bollard::API_DEFAULT_VERSION,
        ) else {
            println!("Docker client unavailable, skipping");
            return;
        };
        let operations = LocalContainerOperations::new(
            Arc::new(docker),
            Arc::new(temps_logs::DockerLogService::new(Arc::new(
                temps_core::DockerHandle::disabled("test", "no Docker in this test"),
            ))),
            Arc::new(UnusedDeployer),
        )
        .with_plane(Some(plane));
        let params = || crate::services::ContainerLogParams {
            start_date: None,
            end_date: None,
            tail: None,
            timestamps: false,
            follow: false,
        };

        let stream = operations
            .stream_logs("vm-42-1", params())
            .await
            .expect("a microVM's logs come from the plane");
        let lines: Vec<String> = stream.map(|line| line.expect("a log line")).collect().await;
        assert_eq!(lines, vec!["Ready in 1440ms\n", "GET / 200\n"]);

        let error = match operations.stream_logs("c-docker", params()).await {
            Ok(_) => panic!("a Docker container's logs must still go to Docker"),
            Err(error) => error,
        };
        assert!(
            matches!(
                error,
                crate::services::DeploymentError::DockerUnavailable(_)
            ),
            "{error}"
        );
    }

    /// A plane that cannot read a replica must not take a Docker
    /// container's logs with it: the read falls through to Docker.
    #[tokio::test]
    async fn a_plane_error_on_the_log_path_falls_through_to_docker() {
        use crate::services::container_operations::{
            ContainerOperations, LocalContainerOperations,
        };
        let Ok(test_db) = temps_database::test_utils::TestDatabase::with_migrations().await else {
            println!("Docker not available, skipping");
            return;
        };
        let db = test_db.connection_arc();
        let plane = Arc::new(
            WorkloadPlane::new(
                db.clone(),
                temps_workload_plane::NodeDriver(None),
                Arc::new(temps_workload_plane::NoEnvSource),
                Arc::new(AcceptingAudit),
                temps_workload_plane::PlaneConfig::new("adr055-log-fallthrough-test"),
            )
            .with_local_driver(Arc::new(FakeMicroVm::with_logs_failing(true))),
        );
        plane_replica(
            &plane,
            db.as_ref(),
            "app-production-42-1",
            Isolation::MicroVm,
            "firecracker",
            "vm-42-1",
        )
        .await;
        let Ok(docker) = bollard::Docker::connect_with_http(
            "http://127.0.0.1:1",
            5,
            bollard::API_DEFAULT_VERSION,
        ) else {
            println!("Docker client unavailable, skipping");
            return;
        };
        let operations = LocalContainerOperations::new(
            Arc::new(docker),
            Arc::new(temps_logs::DockerLogService::new(Arc::new(
                temps_core::DockerHandle::disabled("test", "no Docker in this test"),
            ))),
            Arc::new(UnusedDeployer),
        )
        .with_plane(Some(plane));

        let error = match operations
            .stream_logs(
                "vm-42-1",
                crate::services::ContainerLogParams {
                    start_date: None,
                    end_date: None,
                    tail: None,
                    timestamps: false,
                    follow: false,
                },
            )
            .await
        {
            Ok(_) => panic!("an unreadable guest has no logs to stream"),
            Err(error) => error,
        };
        assert!(
            matches!(
                error,
                crate::services::DeploymentError::DockerUnavailable(_)
            ),
            "the plane's failure must not replace Docker's answer: {error}"
        );
    }

    #[tokio::test]
    async fn a_plane_owned_replica_is_retired_through_the_plane() {
        let Ok(test_db) = temps_database::test_utils::TestDatabase::with_migrations().await else {
            println!("Docker not available, skipping");
            return;
        };
        let db = test_db.connection_arc();
        let plane = Arc::new(WorkloadPlane::new(
            db.clone(),
            temps_workload_plane::NodeDriver(None),
            Arc::new(temps_workload_plane::NoEnvSource),
            Arc::new(AcceptingAudit),
            temps_workload_plane::PlaneConfig::new("adr055-retire-test"),
        ));
        // An enforce-mode replica on a worker.
        let workload_id = plane_replica(
            &plane,
            db.as_ref(),
            "app-production-42-1",
            Isolation::Process,
            "node-stream",
            "c-worker",
        )
        .await;

        // A legacy container, or one of another deployment, is not the
        // plane's: its node's deployer removes it as before.
        assert_eq!(
            deployment_replica_workload(db.as_ref(), 42, "legacy-1")
                .await
                .expect("lookup"),
            None
        );
        assert_eq!(
            deployment_replica_workload(db.as_ref(), 43, "c-worker")
                .await
                .expect("lookup"),
            None
        );

        let found = deployment_replica_workload(db.as_ref(), 42, "c-worker")
            .await
            .expect("lookup")
            .expect("plane-owned");
        assert_eq!(
            found,
            ReplicaWorkload {
                workload_id,
                retired: false
            }
        );

        // Without a plane it is left in place, never assumed gone.
        let error = retire_replica_workload(None, 42, "c-worker", found)
            .await
            .expect_err("no plane");
        assert!(
            matches!(error, ReplicaRetireError::PlaneUnavailable { workload_id: id, .. } if id == workload_id),
            "{error}"
        );
        assert!(error.to_string().contains("c-worker"), "{error}");

        retire_replica_workload(Some(plane.as_ref()), 42, "c-worker", found)
            .await
            .expect("retired");
        let after = deployment_replica_workload(db.as_ref(), 42, "c-worker")
            .await
            .expect("lookup")
            .expect("still recorded");
        assert!(after.retired, "the plane retired the workload");
        // Already retired: nothing left to do, even without a plane.
        retire_replica_workload(None, 42, "c-worker", after)
            .await
            .expect("idempotent");
    }
}
