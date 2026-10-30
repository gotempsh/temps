// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The control plane's half of the ADR-055 §7 node runtime protocol, and
//! where the workload plane places job runs.
//!
//! - [`NodeStateService`]: the stream a worker long-polls
//!   (`GET /internal/nodes/{id}/state?since=W`), its acks, the per-instance
//!   environment call, the heartbeat's instance reports, and the retention
//!   sweep. All of it delegates to the [`WorkloadPlane`], which owns the
//!   tables; this service only exists so handlers can reach the plane, which
//!   is built after the routes are.
//! - [`NodeWorkloadPlacement`]: a job run is placed like a one-replica
//!   deployment of its environment: the environment's `target_nodes` /
//!   `target_labels`, else the project's, through the same
//!   [`NodeScheduler`] eligibility filter and its `LeastLoaded` strategy.
//! - [`AgentApiDriver`]: a worker that does not advertise the
//!   `state_stream` capability (an older agent) is driven through its
//!   existing `/agent/*` API, exactly as deployments drive it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use sea_orm::{DatabaseConnection, EntityTrait};
use temps_core::EncryptionService;
use temps_deployer::remote::RemoteNodeDeployer;
use temps_deployer::{
    ContainerDeployer, ContainerLogConfig, ContainerStatus, DeployRequest, DeployerError,
    ImageBuilder, ResourceLimits,
};
use temps_entities::deployment_config::DeploymentConfig;
use temps_entities::{environments, nodes, projects};
use temps_workload::stream::{
    InstanceEnv, InstanceReport, StateAck, StateResponse, STATE_STREAM_CAPABILITY,
};
use temps_workload::{
    Artifact, ArtifactKinds, AttachmentKind, Attachments, DriverCapabilities, DriverError,
    InstanceHandle, InstanceSpec, InstanceState, InstanceStats, InstanceStatus, Isolation,
    LimitSupport, LogLine, LogQuery, LogStream, PreparedArtifact, RuntimeDriver, SecurityProfile,
};
use temps_workload_plane::{
    parse_cursor, AttemptPlacement, Placed, PlaneError, StreamMetricsSnapshot, StreamNodeDriver,
    WorkloadPlane, WorkloadView, STREAM_DRIVER,
};

use crate::services::{NodeAssignment, NodeScheduler, NodeService, ReplicaPlacementRequest};

/// The longest a stream read waits for a change before answering empty.
pub const MAX_STREAM_WAIT: Duration = Duration::from_secs(25);
/// How often a waiting stream read looks again.
const STREAM_POLL: Duration = Duration::from_secs(2);
/// How often the retention sweep runs.
const SWEEP_INTERVAL: Duration = Duration::from_secs(300);
/// A node whose heartbeat is older than this holds no stream markers back.
const ACTIVE_NODE_SECS: i64 = 90;
/// How long a node's driver is reused before it is rebuilt (a rotated
/// token, a re-registered address).
const DRIVER_TTL: Duration = Duration::from_secs(60);
/// The driver name of instances run through a worker's `/agent/*` API.
pub const AGENT_API_DRIVER: &str = "agent-api";

#[derive(Debug, thiserror::Error)]
pub enum NodeStateError {
    #[error(
        "the workload plane is not running in this process, so node {node_id} cannot \
         {operation}; job crons report why on their runs"
    )]
    PlaneNotRunning {
        node_id: i32,
        operation: &'static str,
    },
    #[error("node {node_id}: {reason}")]
    InvalidCursor { node_id: i32, reason: String },
    #[error("instance {instance_id} is not an unfinished instance assigned to node {node_id}")]
    InstanceNotOnNode { node_id: i32, instance_id: i64 },
    #[error("node {node_id}: {operation} failed: {source}")]
    Plane {
        node_id: i32,
        operation: &'static str,
        // Boxed: a plane error is large and this enum rides every
        // stream read's `Result`.
        #[source]
        source: Box<PlaneError>,
    },
}

impl NodeStateError {
    fn plane(node_id: i32, operation: &'static str) -> impl FnOnce(PlaneError) -> Self {
        move |source| match source {
            PlaneError::InstanceNotOnNode {
                instance_id,
                node_id,
            } => NodeStateError::InstanceNotOnNode {
                node_id,
                instance_id,
            },
            source => NodeStateError::Plane {
                node_id,
                operation,
                source: Box::new(source),
            },
        }
    }
}

/// What the status endpoint reports about the stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeStateStatus {
    /// Whether the workload plane runs in this process.
    pub running: bool,
    /// Why not, when `running` is false.
    pub reason: Option<String>,
    pub metrics: Option<StreamMetricsSnapshot>,
}

/// The node state stream, for handlers. See the module docs.
#[derive(Default)]
pub struct NodeStateService {
    plane: OnceLock<Arc<WorkloadPlane>>,
}

impl NodeStateService {
    pub fn new() -> Self {
        Self::default()
    }

    /// Hand over the started plane. Later calls are ignored: a process runs
    /// one plane.
    pub fn set_plane(&self, plane: Arc<WorkloadPlane>) {
        if self.plane.set(plane).is_err() {
            tracing::warn!("node state service already has a workload plane; ignored");
        }
    }

    fn plane(
        &self,
        node_id: i32,
        operation: &'static str,
    ) -> Result<&Arc<WorkloadPlane>, NodeStateError> {
        self.plane
            .get()
            .ok_or(NodeStateError::PlaneNotRunning { node_id, operation })
    }

    /// One long-poll read of node `node_id`'s stream: answers at once with
    /// any change or full snapshot, else waits up to `wait` (capped at
    /// [`MAX_STREAM_WAIT`]) for one.
    pub async fn read(
        &self,
        node_id: i32,
        since: Option<&str>,
        wait: Duration,
    ) -> Result<StateResponse, NodeStateError> {
        let plane = self.plane(node_id, "read its state stream")?;
        let since = parse_cursor(since)
            .map_err(|reason| NodeStateError::InvalidCursor { node_id, reason })?;
        let deadline = Instant::now() + wait.min(MAX_STREAM_WAIT);
        loop {
            let answer = plane
                .read_node_state(node_id, since)
                .await
                .map_err(NodeStateError::plane(node_id, "reading the state stream"))?;
            let waited_enough = Instant::now() + STREAM_POLL > deadline;
            if answer.full.is_some() || !answer.changes.is_empty() || answer.more || waited_enough {
                return Ok(answer);
            }
            tokio::time::sleep(STREAM_POLL).await;
        }
    }

    pub async fn ack(&self, node_id: i32, ack: &StateAck) -> Result<(), NodeStateError> {
        self.plane(node_id, "acknowledge its state stream")?
            .ack_node_state(node_id, ack)
            .await
            .map_err(NodeStateError::plane(node_id, "recording a stream ack"))
    }

    /// The environment of one of the node's instances. Carries secret
    /// values: never logged, never cached by the caller.
    pub async fn instance_env(
        &self,
        node_id: i32,
        instance_id: i64,
    ) -> Result<InstanceEnv, NodeStateError> {
        self.plane(node_id, "read an instance environment")?
            .instance_env(node_id, instance_id)
            .await
            .map_err(NodeStateError::plane(
                node_id,
                "resolving an instance environment",
            ))
    }

    /// The instance reports of a heartbeat. Returns how many rows changed.
    pub async fn ingest(
        &self,
        node_id: i32,
        reports: &[InstanceReport],
    ) -> Result<usize, NodeStateError> {
        if reports.is_empty() {
            return Ok(0);
        }
        self.plane(node_id, "report its instances")?
            .ingest_node_reports(node_id, reports)
            .await
            .map_err(NodeStateError::plane(node_id, "recording instance reports"))
    }

    pub fn status(&self) -> NodeStateStatus {
        match self.plane.get() {
            Some(plane) => NodeStateStatus {
                running: true,
                reason: None,
                metrics: Some(plane.stream_metrics().snapshot()),
            },
            None => NodeStateStatus {
                running: false,
                reason: Some(
                    "the workload plane did not start in this process (no audit logger or no \
                     cron scheduler); see the server log"
                        .to_string(),
                ),
                metrics: None,
            },
        }
    }

    /// Start the retention sweep: every 5 minutes, delete the markers every
    /// active node acknowledged and every marker older than 24 h.
    pub fn start_retention(self: &Arc<Self>, node_service: Arc<NodeService>) {
        let service = self.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(SWEEP_INTERVAL);
            loop {
                interval.tick().await;
                let Some(plane) = service.plane.get() else {
                    continue;
                };
                let active: Vec<i32> = match node_service.list_active(ACTIVE_NODE_SECS).await {
                    Ok(nodes) => nodes.into_iter().map(|node| node.id).collect(),
                    Err(error) => {
                        tracing::warn!(
                            "node state retention: listing active nodes failed, sweep skipped: \
                             {error}"
                        );
                        continue;
                    }
                };
                match plane.sweep_node_state(&active, chrono::Utc::now()).await {
                    Ok(outcome) if outcome.deleted > 0 => tracing::info!(
                        deleted = outcome.deleted,
                        horizon = %outcome.horizon,
                        active_nodes = active.len(),
                        "node state retention swept stream markers"
                    ),
                    Ok(_) => {}
                    Err(error) => {
                        tracing::warn!("node state retention sweep failed: {error}")
                    }
                }
            }
        });
    }
}

/// Whether a node's last heartbeat advertised the `state_stream`
/// capability (`capacity.node_capabilities`).
pub fn advertises_state_stream(capacity: &serde_json::Value) -> bool {
    capacity
        .get("node_capabilities")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|capabilities| {
            capabilities
                .iter()
                .any(|c| c.as_str() == Some(STATE_STREAM_CAPABILITY))
        })
}

fn agent_capabilities() -> DriverCapabilities {
    DriverCapabilities {
        name: AGENT_API_DRIVER.to_string(),
        artifacts: ArtifactKinds {
            oci: true,
            wasm: false,
            rootfs: false,
        },
        isolation: Isolation::Process,
        network_attachment: AttachmentKind::DockerNetwork,
        volumes: Default::default(),
        exec: false,
        snapshot: false,
        resize_disk: false,
        limits: LimitSupport {
            cpu: true,
            memory: true,
            pids: false,
            disk: false,
        },
    }
}

fn oci_reference(artifact: &Artifact) -> Result<String, DriverError> {
    match artifact {
        Artifact::OciImage { reference, digest } => Ok(match digest {
            Some(digest) => format!(
                "{}@{digest}",
                reference.split('@').next().unwrap_or(reference)
            ),
            None => reference.clone(),
        }),
        _ => Err(DriverError::Unsupported {
            driver: AGENT_API_DRIVER.to_string(),
            operation: "non-OCI artifacts",
            instance: String::new(),
        }),
    }
}

/// A worker driven through its `/agent/*` API (agents without the
/// `state_stream` capability, and every workload holding the ADR-045
/// socket, whose grant the executing agent decides).
pub struct AgentApiDriver {
    node_id: i32,
    node_name: String,
    remote: Arc<RemoteNodeDeployer>,
    /// The control plane's image store, to hand an image that exists only
    /// here (built or uploaded on the control plane) to the worker.
    local_images: Option<Arc<dyn ImageBuilder>>,
    /// The project slug and the control plane's ADR-045 declaration, sent
    /// with a create so the agent answers from its own grant.
    project_slug: Option<String>,
    control_plane_grants_socket: bool,
    capabilities: DriverCapabilities,
    poll: Duration,
}

impl AgentApiDriver {
    pub fn new(
        node_id: i32,
        node_name: String,
        remote: Arc<RemoteNodeDeployer>,
        local_images: Option<Arc<dyn ImageBuilder>>,
    ) -> Self {
        Self {
            node_id,
            node_name,
            remote,
            local_images,
            project_slug: None,
            control_plane_grants_socket: false,
            capabilities: agent_capabilities(),
            poll: Duration::from_secs(5),
        }
    }

    pub fn with_project(mut self, slug: Option<String>, control_plane_grants_socket: bool) -> Self {
        self.project_slug = slug;
        self.control_plane_grants_socket = control_plane_grants_socket;
        self
    }

    fn runtime_error(
        &self,
        operation: &'static str,
        instance: &str,
        error: DeployerError,
    ) -> DriverError {
        DriverError::Runtime {
            driver: AGENT_API_DRIVER.to_string(),
            operation,
            instance: instance.to_string(),
            reason: format!("node {} ('{}'): {error}", self.node_id, self.node_name),
        }
    }

    /// The request the agent's deployer creates and starts the container
    /// from. Fails for what the `/agent/*` API cannot express.
    pub(crate) fn deploy_request(
        &self,
        spec: &InstanceSpec,
        attach: &Attachments,
    ) -> Result<DeployRequest, DriverError> {
        let unsupported = |operation: &'static str| DriverError::Unsupported {
            driver: AGENT_API_DRIVER.to_string(),
            operation,
            instance: spec.name.clone(),
        };
        if spec.workdir.is_some() {
            return Err(unsupported("a working directory override"));
        }
        if spec.user.is_some() {
            return Err(unsupported("a user override"));
        }
        let restart_policy = match spec.restart {
            temps_workload::RestartPolicy::Never => temps_deployer::RestartPolicy::Never,
            temps_workload::RestartPolicy::Always => temps_deployer::RestartPolicy::Always,
            temps_workload::RestartPolicy::OnFailure => temps_deployer::RestartPolicy::OnFailure,
            temps_workload::RestartPolicy::UnlessStopped => {
                temps_deployer::RestartPolicy::UnlessStopped
            }
        };
        const MIB: i64 = 1024 * 1024;
        Ok(DeployRequest {
            image_name: spec.image.clone(),
            container_name: spec.name.clone(),
            environment_vars: spec.env.iter().cloned().collect(),
            secrets: spec
                .secret_files
                .iter()
                .map(|file| (file.name.clone(), file.value.clone()))
                .collect(),
            // The agent binds them to its own private address and picks the
            // host port; `inspect` reads back what it got.
            port_mappings: spec
                .ports
                .iter()
                .map(|port| temps_deployer::PortMapping {
                    host_port: port.host_port.unwrap_or(0),
                    container_port: port.container_port,
                    protocol: match port.protocol {
                        temps_workload::PortProtocol::Tcp => temps_deployer::Protocol::Tcp,
                        temps_workload::PortProtocol::Udp => temps_deployer::Protocol::Udp,
                    },
                    host_ip: None,
                })
                .collect(),
            network_name: attach.network.primary.clone(),
            extra_networks: attach.network.required.clone(),
            resource_limits: ResourceLimits {
                cpu_limit: spec.limits.nano_cpus.map(|n| n as f64 / 1_000_000_000.0),
                memory_limit_mb: spec
                    .limits
                    .memory_bytes
                    .and_then(|bytes| u64::try_from(bytes / MIB).ok()),
                disk_limit_mb: None,
            },
            restart_policy,
            log_path: std::env::temp_dir().join(format!("{}.log", spec.name)),
            command: spec.command.clone(),
            entrypoint: spec.entrypoint.clone(),
            log_config: spec
                .log_rotation
                .as_ref()
                .map(|r| ContainerLogConfig::new(r.max_size.clone(), r.max_file)),
            labels: agent_labels(spec),
            project_slug: self.project_slug.clone(),
            control_plane_grants_socket: self.control_plane_grants_socket
                && spec.security == SecurityProfile::HostPrivileged,
        })
    }
}

/// The label every agent's `/agent/containers/{id}/*` endpoints gate on:
/// a container without it reads as absent and cannot be stopped or removed
/// through the API, so an instance the plane created would look lost.
const AGENT_MANAGED_LABEL: &str = "sh.temps.managed";

/// The instance's labels plus the agent's ownership marker.
fn agent_labels(spec: &InstanceSpec) -> HashMap<String, String> {
    let mut labels: HashMap<String, String> = spec
        .labels
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    labels.insert(AGENT_MANAGED_LABEL.to_string(), "true".to_string());
    labels
}

fn instance_state(status: &ContainerStatus) -> InstanceState {
    match status {
        ContainerStatus::Created => InstanceState::Created,
        ContainerStatus::Running | ContainerStatus::Paused => InstanceState::Running,
        ContainerStatus::Stopped | ContainerStatus::Exited | ContainerStatus::Dead => {
            InstanceState::Exited
        }
    }
}

#[async_trait]
impl RuntimeDriver for AgentApiDriver {
    fn capabilities(&self) -> &DriverCapabilities {
        &self.capabilities
    }

    /// On the worker already, else from the control plane's own copy, else
    /// pulled by the worker from its registry.
    async fn prepare(&self, artifact: &Artifact) -> Result<PreparedArtifact, DriverError> {
        let reference = oci_reference(artifact)?;
        let failed = |reason: String| DriverError::Artifact {
            driver: AGENT_API_DRIVER.to_string(),
            artifact: reference.clone(),
            reason: format!("node {} ('{}'): {reason}", self.node_id, self.node_name),
        };
        if let Ok(true) = self.remote.image_exists(&reference).await {
            return Ok(PreparedArtifact {
                artifact: artifact.clone(),
                local_ref: reference,
            });
        }
        if let Some(local) = &self.local_images {
            if local.inspect_image(&reference).await.is_ok() {
                let stream = local
                    .export_image_stream(&reference)
                    .await
                    .map_err(|e| failed(format!("exporting it from the control plane: {e}")))?;
                ImageBuilder::import_image_stream(self.remote.as_ref(), stream, &reference)
                    .await
                    .map_err(|e| failed(format!("importing it on the node: {e}")))?;
                tracing::info!(
                    node_id = self.node_id,
                    image = %reference,
                    "moved the control plane's image to the node for a workload"
                );
                return Ok(PreparedArtifact {
                    artifact: artifact.clone(),
                    local_ref: reference,
                });
            }
        }
        self.remote
            .pull_image_from_registry(&reference, None)
            .await
            .map_err(|e| failed(format!("pulling it on the node: {e}")))?;
        Ok(PreparedArtifact {
            artifact: artifact.clone(),
            local_ref: reference,
        })
    }

    async fn create(
        &self,
        spec: &InstanceSpec,
        attach: &Attachments,
    ) -> Result<InstanceHandle, DriverError> {
        let request = self.deploy_request(spec, attach)?;
        let result = self
            .remote
            .deploy_container(request)
            .await
            .map_err(|e| self.runtime_error("create", &spec.name, e))?;
        if result.docker_socket_mounted {
            tracing::warn!(
                event = "docker_socket_mounted",
                node_id = self.node_id,
                container = %spec.name,
                "the node mounted the host Docker socket into a workload instance (ADR 045)"
            );
        }
        Ok(InstanceHandle {
            driver: AGENT_API_DRIVER.to_string(),
            id: result.container_id,
            name: spec.name.clone(),
        })
    }

    /// The agent's deploy call starts the container; only a container left
    /// created is started here.
    async fn start(&self, handle: &InstanceHandle) -> Result<(), DriverError> {
        if self.inspect(handle).await?.state == InstanceState::Created {
            self.remote
                .start_container(&handle.id)
                .await
                .map_err(|e| self.runtime_error("start", &handle.name, e))?;
        }
        Ok(())
    }

    async fn stop(&self, handle: &InstanceHandle) -> Result<(), DriverError> {
        match self.remote.stop_container(&handle.id).await {
            Ok(()) | Err(DeployerError::ContainerNotFound(_)) => Ok(()),
            Err(e) => Err(self.runtime_error("stop", &handle.name, e)),
        }
    }

    async fn remove(&self, handle: &InstanceHandle, _purge: bool) -> Result<(), DriverError> {
        match self.remote.remove_container(&handle.id).await {
            Ok(()) | Err(DeployerError::ContainerNotFound(_)) => Ok(()),
            Err(e) => Err(self.runtime_error("remove", &handle.name, e)),
        }
    }

    async fn inspect(&self, handle: &InstanceHandle) -> Result<InstanceStatus, DriverError> {
        match self.remote.get_container_info(&handle.id).await {
            Ok(info) => Ok(InstanceStatus {
                state: instance_state(&info.status),
                exit_code: info.exit_code.map(i64::from),
                started_at: None,
                finished_at: None,
                address: None,
                published_ports: info
                    .ports
                    .iter()
                    .filter(|port| port.host_port != 0)
                    .map(|port| temps_workload::PortBinding {
                        container_port: port.container_port,
                        protocol: match port.protocol {
                            temps_deployer::Protocol::Tcp => temps_workload::PortProtocol::Tcp,
                            temps_deployer::Protocol::Udp => temps_workload::PortProtocol::Udp,
                        },
                        host_ip: port.host_ip.clone().unwrap_or_default(),
                        host_port: Some(port.host_port),
                    })
                    .collect(),
            }),
            Err(DeployerError::ContainerNotFound(_)) => Ok(InstanceStatus {
                state: InstanceState::Missing,
                exit_code: None,
                started_at: None,
                finished_at: None,
                address: None,
                published_ports: Vec::new(),
            }),
            Err(e) => Err(self.runtime_error("inspect", &handle.name, e)),
        }
    }

    async fn logs(
        &self,
        handle: &InstanceHandle,
        query: LogQuery,
    ) -> Result<LogStream, DriverError> {
        let text = self
            .remote
            .get_container_logs(&handle.id)
            .await
            .map_err(|e| self.runtime_error("read the logs of", &handle.name, e))?;
        let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
        if let Some(tail) = query.tail {
            let keep = usize::try_from(tail).unwrap_or(usize::MAX);
            if lines.len() > keep {
                lines.drain(..lines.len() - keep);
            }
        }
        let items = lines.into_iter().map(|line| {
            Ok(LogLine {
                stderr: false,
                line: bytes::Bytes::from(line + "\n"),
            })
        });
        Ok(Box::pin(futures::stream::iter(items)))
    }

    async fn stats(&self, handle: &InstanceHandle) -> Result<InstanceStats, DriverError> {
        let stats = self
            .remote
            .get_container_stats(&handle.id)
            .await
            .map_err(|e| self.runtime_error("read the stats of", &handle.name, e))?;
        Ok(InstanceStats {
            cpu_nanos: None,
            memory_bytes: Some(stats.memory_bytes),
            pids: None,
            net_rx_bytes: Some(stats.network_rx_bytes),
            net_tx_bytes: Some(stats.network_tx_bytes),
            memory_limit_bytes: stats.memory_limit_bytes,
            ..InstanceStats::default()
        })
    }

    async fn wait(&self, handle: &InstanceHandle) -> Result<(), DriverError> {
        loop {
            match self.inspect(handle).await?.state {
                InstanceState::Created | InstanceState::Running => {}
                _ => return Ok(()),
            }
            tokio::time::sleep(self.poll).await;
        }
    }
}

/// Places the workload plane's job runs on nodes (see the module docs).
pub struct NodeWorkloadPlacement {
    db: Arc<DatabaseConnection>,
    scheduler: Arc<NodeScheduler>,
    config_service: Arc<temps_config::ConfigService>,
    encryption_service: Arc<EncryptionService>,
    local_images: Option<Arc<dyn ImageBuilder>>,
    drivers: Mutex<HashMap<DriverKey, CachedDriver>>,
}

/// A cached driver's key: the node and the driver name.
type DriverKey = (i32, String);
/// A cached driver and when it was built (entries expire after [`DRIVER_TTL`]).
type CachedDriver = (Instant, Arc<dyn RuntimeDriver>);

/// Where a job's environment says it may run.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct JobTargets {
    pub target_nodes: Option<Vec<i32>>,
    pub target_labels: Option<serde_json::Value>,
    pub project_slug: Option<String>,
}

/// The environment's targets, else the project's.
pub(crate) fn job_targets(
    environment_config: Option<&DeploymentConfig>,
    project_config: Option<&DeploymentConfig>,
    project_slug: Option<String>,
) -> JobTargets {
    JobTargets {
        target_nodes: environment_config
            .and_then(|c| c.configured_target_nodes().map(<[i32]>::to_vec))
            .or_else(|| {
                project_config.and_then(|c| c.configured_target_nodes().map(<[i32]>::to_vec))
            }),
        target_labels: environment_config
            .and_then(|c| c.configured_target_labels().cloned())
            .or_else(|| project_config.and_then(|c| c.configured_target_labels().cloned())),
        project_slug,
    }
}

impl NodeWorkloadPlacement {
    pub fn new(
        db: Arc<DatabaseConnection>,
        scheduler: Arc<NodeScheduler>,
        config_service: Arc<temps_config::ConfigService>,
        encryption_service: Arc<EncryptionService>,
        local_images: Option<Arc<dyn ImageBuilder>>,
    ) -> Self {
        Self {
            db,
            scheduler,
            config_service,
            encryption_service,
            local_images,
            drivers: Mutex::new(HashMap::new()),
        }
    }

    async fn targets(&self, workload: &WorkloadView) -> Result<JobTargets, String> {
        let Some(environment_id) = workload
            .spec
            .labels
            .get("sh.temps.environment")
            .and_then(|v| v.parse::<i32>().ok())
        else {
            return Ok(JobTargets::default());
        };
        let found = environments::Entity::find_by_id(environment_id)
            .find_also_related(projects::Entity)
            .one(self.db.as_ref())
            .await
            .map_err(|e| {
                format!(
                    "reading environment {environment_id} of workload {} to place it: {e}",
                    workload.id
                )
            })?;
        Ok(match found {
            Some((environment, project)) => job_targets(
                environment.deployment_config.as_ref(),
                project.as_ref().and_then(|p| p.deployment_config.as_ref()),
                project.as_ref().map(|p| p.slug.clone()),
            ),
            None => JobTargets::default(),
        })
    }

    async fn remote(&self, node: &nodes::Model) -> Result<Arc<RemoteNodeDeployer>, String> {
        let encrypted = node.token_encrypted.as_ref().ok_or_else(|| {
            format!(
                "node {} ('{}') has no agent token; cannot reach its agent",
                node.id, node.name
            )
        })?;
        let token = self
            .encryption_service
            .decrypt(encrypted)
            .map_err(|e| format!("decrypting the agent token of node {}: {e}", node.id))
            .and_then(|bytes| {
                String::from_utf8(bytes)
                    .map_err(|e| format!("the agent token of node {} is not utf-8: {e}", node.id))
            })?;
        let remote = crate::cluster_ca::build_node_deployer(
            node,
            token,
            self.config_service.as_ref(),
            self.encryption_service.as_ref(),
        )
        .await
        .map_err(|e| format!("building the agent client of node {}: {e}", node.id))?;
        Ok(Arc::new(remote))
    }

    async fn build_driver(
        &self,
        node: &nodes::Model,
        driver: &str,
        project: Option<(Option<String>, bool)>,
    ) -> Result<Arc<dyn RuntimeDriver>, String> {
        let mut agent = AgentApiDriver::new(
            node.id,
            node.name.clone(),
            self.remote(node).await?,
            self.local_images.clone(),
        );
        if let Some((slug, grants)) = project {
            agent = agent.with_project(slug, grants);
        }
        let agent: Arc<dyn RuntimeDriver> = Arc::new(agent);
        if driver == STREAM_DRIVER {
            Ok(Arc::new(StreamNodeDriver::new(
                self.db.clone(),
                node.id,
                agent,
            )))
        } else {
            Ok(agent)
        }
    }

    async fn node(&self, node_id: i32) -> Result<nodes::Model, String> {
        nodes::Entity::find_by_id(node_id)
            .one(self.db.as_ref())
            .await
            .map_err(|e| format!("reading node {node_id}: {e}"))?
            .ok_or_else(|| format!("node {node_id} no longer exists"))
    }
}

#[async_trait]
impl AttemptPlacement for NodeWorkloadPlacement {
    async fn place(&self, workload: &WorkloadView) -> Result<Placed, String> {
        let targets = self.targets(workload).await?;
        // A spec's own pin (a backup job on its database's node, ADR-055
        // Phase 9) wins over the environment's targets; the plane refuses
        // a placement outside it.
        let pinned = workload.spec.placement.pinned_nodes();
        let outcome = self
            .scheduler
            .schedule_placement(ReplicaPlacementRequest {
                replica_count: 1,
                labels: if pinned.is_some() {
                    None
                } else {
                    targets.target_labels.as_ref()
                },
                target_node_ids: pinned.or(targets.target_nodes.as_deref()),
                anti_affinity: false,
                exclude_node_ids: &[],
                image_platforms: &[],
                project_slug: targets.project_slug.as_deref(),
                exclude_control_plane: false,
            })
            .await
            .map_err(|e| format!("no node is eligible for workload {}: {e}", workload.id))?;
        let Some(assignment) = outcome.assignments.into_iter().next() else {
            let excluded: Vec<String> = outcome
                .exclusions
                .iter()
                .map(|e| format!("{} ({:?})", e.node_name, e.reason))
                .collect();
            return Err(format!(
                "0 nodes eligible for workload {}; excluded: {}",
                workload.id,
                if excluded.is_empty() {
                    "none".to_string()
                } else {
                    excluded.join(", ")
                }
            ));
        };
        match assignment {
            NodeAssignment::Local => Ok(Placed::ControlPlane),
            NodeAssignment::Remote {
                node_id, node_name, ..
            } => {
                let node = self.node(node_id).await?;
                // The ADR-045 socket is decided by the executing agent from
                // its own grant, which only its `/agent/*` deploy call
                // carries: a socket workload never goes through the stream.
                let privileged = workload.spec.security == Some(SecurityProfile::HostPrivileged);
                let driver = if advertises_state_stream(&node.capacity) && !privileged {
                    STREAM_DRIVER
                } else {
                    AGENT_API_DRIVER
                };
                let driver = self
                    .build_driver(
                        &node,
                        driver,
                        Some((targets.project_slug, outcome.docker_socket_required)),
                    )
                    .await?;
                Ok(Placed::Node {
                    node_id,
                    node_name,
                    driver,
                })
            }
        }
    }

    /// A deployment replica the deploy job already placed on `node_id`:
    /// through the stream when the node advertises it, else (and for an
    /// ADR-045 socket workload, which only `/agent/*` mounts) `/agent/*`.
    async fn place_on(&self, workload: &WorkloadView, node_id: i32) -> Result<Placed, String> {
        let targets = self.targets(workload).await?;
        let node = self.node(node_id).await?;
        let privileged = workload.spec.security == Some(SecurityProfile::HostPrivileged);
        let driver = if advertises_state_stream(&node.capacity) && !privileged {
            STREAM_DRIVER
        } else {
            AGENT_API_DRIVER
        };
        let driver = self
            .build_driver(&node, driver, Some((targets.project_slug, privileged)))
            .await?;
        Ok(Placed::Node {
            node_id,
            node_name: node.name,
            driver,
        })
    }

    async fn driver_for(
        &self,
        node_id: i32,
        driver: &str,
    ) -> Result<Arc<dyn RuntimeDriver>, String> {
        let key = (node_id, driver.to_string());
        if let Some((built, cached)) = self
            .drivers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&key)
        {
            if built.elapsed() < DRIVER_TTL {
                return Ok(cached.clone());
            }
        }
        let node = self.node(node_id).await?;
        let built = self.build_driver(&node, driver, None).await?;
        self.drivers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(key, (Instant::now(), built.clone()));
        Ok(built)
    }

    async fn node_exists(&self, node_id: i32) -> Result<bool, String> {
        nodes::Entity::find_by_id(node_id)
            .one(self.db.as_ref())
            .await
            .map(|node| node.is_some())
            .map_err(|e| format!("reading node {node_id}: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use temps_workload::{
        InstanceLimits, LogRotation, NetworkAttachment, WorkloadKind, WorkloadRef,
    };

    fn spec() -> InstanceSpec {
        InstanceSpec {
            owner: WorkloadRef::new(WorkloadKind::Job, 3),
            name: "temps-job-9-a1".into(),
            image: "registry.example.test/app:1".into(),
            platform: None,
            env: vec![("APP_MODE".into(), "worker".into())],
            secret_files: Vec::new(),
            command: Some(vec!["bin/cleanup".into()]),
            entrypoint: None,
            workdir: None,
            user: None,
            ports: Vec::new(),
            restart: temps_workload::RestartPolicy::Never,
            limits: InstanceLimits {
                nano_cpus: Some(500_000_000),
                memory_bytes: Some(256 * 1024 * 1024),
                ..Default::default()
            },
            log_rotation: Some(LogRotation {
                max_size: "10m".into(),
                max_file: 3,
            }),
            labels: BTreeMap::from([("sh.temps.project_id".into(), "4".into())]),
            security: SecurityProfile::Trusted,
        }
    }

    fn driver() -> AgentApiDriver {
        let remote = RemoteNodeDeployer::new(
            "http://10.233.0.9:3100".into(),
            "token".into(),
            "worker-a".into(),
        )
        .expect("client");
        AgentApiDriver::new(2, "worker-a".into(), Arc::new(remote), None)
    }

    #[test]
    fn a_job_instance_becomes_an_agent_deploy_request() {
        let attach = Attachments {
            network: NetworkAttachment {
                primary: Some("temps".into()),
                required: vec!["db-net".into()],
                ..Default::default()
            },
            volumes: Vec::new(),
        };
        let request = driver().deploy_request(&spec(), &attach).expect("request");
        assert_eq!(request.container_name, "temps-job-9-a1");
        assert_eq!(request.network_name.as_deref(), Some("temps"));
        assert_eq!(request.extra_networks, vec!["db-net".to_string()]);
        assert!(matches!(
            request.restart_policy,
            temps_deployer::RestartPolicy::Never
        ));
        assert_eq!(request.resource_limits.memory_limit_mb, Some(256));
        assert_eq!(request.resource_limits.cpu_limit, Some(0.5));
        assert_eq!(
            request.environment_vars.get("APP_MODE").map(String::as_str),
            Some("worker")
        );
        assert!(!request.control_plane_grants_socket);
        // Without the ownership marker every agent's info/stop/remove
        // endpoints report the container as absent and the run as lost.
        assert_eq!(
            request.labels.get("sh.temps.managed").map(String::as_str),
            Some("true")
        );
        assert_eq!(
            request
                .labels
                .get("sh.temps.project_id")
                .map(String::as_str),
            Some("4")
        );
    }

    #[test]
    fn dockerfile_runtime_overrides_reach_the_agent() {
        for entrypoint in [None, Some(vec![]), Some(vec!["/app/start".into()])] {
            let mut spec = spec();
            spec.entrypoint = entrypoint.clone();
            let request = driver()
                .deploy_request(&spec, &Attachments::default())
                .unwrap();
            assert_eq!(request.command, spec.command);
            assert_eq!(request.entrypoint, entrypoint);
        }
    }

    #[test]
    fn what_the_agent_api_cannot_express_is_refused_by_name() {
        let mut with_workdir = spec();
        with_workdir.workdir = Some("/app".into());
        let error = driver()
            .deploy_request(&with_workdir, &Attachments::default())
            .expect_err("refused");
        assert!(error.to_string().contains("working directory"), "{error}");
        assert!(error.to_string().contains("temps-job-9-a1"), "{error}");
    }

    #[test]
    fn only_a_socket_workload_carries_the_control_plane_declaration() {
        let declared = driver().with_project(Some("infra-agent".into()), true);
        let plain = declared
            .deploy_request(&spec(), &Attachments::default())
            .expect("request");
        assert!(!plain.control_plane_grants_socket);
        assert_eq!(plain.project_slug.as_deref(), Some("infra-agent"));
        let mut privileged = spec();
        privileged.security = SecurityProfile::HostPrivileged;
        let request = declared
            .deploy_request(&privileged, &Attachments::default())
            .expect("request");
        assert!(request.control_plane_grants_socket);
    }

    #[test]
    fn the_capability_is_read_from_the_heartbeat_capacity() {
        assert!(advertises_state_stream(&serde_json::json!({
            "cpu_usage": 3.0,
            "node_capabilities": ["state_stream"]
        })));
        assert!(!advertises_state_stream(
            &serde_json::json!({ "cpu_usage": 3.0 })
        ));
        assert!(!advertises_state_stream(&serde_json::json!({
            "node_capabilities": "state_stream"
        })));
    }

    #[test]
    fn job_targets_take_the_environment_then_the_project() {
        let config =
            |nodes: Option<Vec<i32>>, labels: Option<serde_json::Value>| DeploymentConfig {
                target_nodes: nodes,
                target_labels: labels,
                ..Default::default()
            };
        let project = config(Some(vec![3]), Some(serde_json::json!({"zone": "a"})));
        let environment = config(Some(vec![5]), None);
        let targets = job_targets(Some(&environment), Some(&project), Some("shop".into()));
        assert_eq!(targets.target_nodes, Some(vec![5]));
        assert_eq!(
            targets.target_labels,
            Some(serde_json::json!({"zone": "a"}))
        );
        assert_eq!(targets.project_slug.as_deref(), Some("shop"));

        // An empty override is the API's "inherit" sentinel.
        let inherit = config(Some(vec![]), Some(serde_json::json!({})));
        let targets = job_targets(Some(&inherit), Some(&project), None);
        assert_eq!(targets.target_nodes, Some(vec![3]));
        assert_eq!(
            targets.target_labels,
            Some(serde_json::json!({"zone": "a"}))
        );
        assert_eq!(job_targets(None, None, None), JobTargets::default());
    }
}
