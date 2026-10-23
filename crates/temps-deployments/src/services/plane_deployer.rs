// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! A [`ContainerDeployer`] that creates a deployment's control-plane replica
//! through the workload plane (ADR-055 Phase 5, `workload_mode = enforce`).
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
//! The deployment's variables are staged in memory
//! ([`WorkloadPlane::stage_env`]) and never written into the stored spec.
//!
//! What the plane cannot yet deliver is refused up front with the reason,
//! never dropped silently: secret files (`/run/secrets`) and the ADR-045
//! Docker socket. Such an environment stays on `legacy` or `shadow`.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use temps_deployer::{
    ContainerDeployer, ContainerInfo, ContainerStatus, DeployRequest, DeployResult, DeployerError,
};
use temps_workload::{
    Artifact, ConditionKind, EnvSpec, Lifetime, NetworkSpec, PlacementSpec, PortProtocol, PortSpec,
    Process, ResourceAmounts, Resources, WorkloadKind, WorkloadRef, WorkloadSpec,
};
use temps_workload_plane::scheduler::CONTROL_PLANE_NODE_ID;
use temps_workload_plane::{ApplyRequest, Owner, WorkloadPlane};

/// Owner kind of a deployment's workloads.
pub const DEPLOYMENT_OWNER_KIND: &str = "deployment";

/// The `Deployment` workload one replica's create request becomes: pinned
/// to the control plane (the job already placed it there), long-running
/// with the request's restart policy, its variables imported from
/// `staged_import`, and its CPU/memory caps carried over as limits.
pub fn deployment_workload_spec(
    request: &DeployRequest,
    deployment_id: i32,
    staged_import: String,
) -> Result<WorkloadSpec, DeployerError> {
    if !request.secrets.is_empty() {
        return Err(DeployerError::DeploymentFailed(format!(
            "Container {} has {} secret file(s); the workload plane cannot deliver secret files \
             yet. Set this environment's workload_mode back to legacy (or shadow) to deploy it",
            request.container_name,
            request.secrets.len()
        )));
    }
    if request.control_plane_grants_socket {
        return Err(DeployerError::DeploymentFailed(format!(
            "Container {} is for project '{}', which is granted the host Docker socket (ADR 045); \
             the workload plane does not mount it for deployments yet. Set this environment's \
             workload_mode back to legacy (or shadow) to deploy it",
            request.container_name,
            request.project_slug.as_deref().unwrap_or("<unknown>")
        )));
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
        isolation: Default::default(),
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
            nodes: Some(vec![CONTROL_PLANE_NODE_ID]),
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

/// Creates one deployment's control-plane replicas through the plane.
pub struct PlaneContainerDeployer {
    plane: Arc<WorkloadPlane>,
    /// The local deployer, for every operation after the create.
    local: Arc<dyn ContainerDeployer>,
    deployment_id: i32,
    ready_timeout: Duration,
    /// Container id -> workload id, for retiring a replica through the
    /// plane. One deploy job's replicas: a handful of entries.
    workloads: Mutex<HashMap<String, i64>>,
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
            ready_timeout,
            workloads: Mutex::new(HashMap::new()),
        }
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
        let staged = self.plane.stage_env(
            &format!(
                "deployment-{}-{}",
                self.deployment_id, request.container_name
            ),
            request
                .environment_vars
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
        );
        let spec = deployment_workload_spec(&request, self.deployment_id, staged.import())?;
        let container_port = request
            .port_mappings
            .first()
            .map(|mapping| mapping.container_port)
            .unwrap_or(0);
        let view = self
            .plane
            .apply_and_wait(
                ApplyRequest {
                    owner: Owner::new(DEPLOYMENT_OWNER_KIND, i64::from(self.deployment_id)),
                    name: request.container_name.clone(),
                    spec,
                    project_slug: request.project_slug.clone(),
                    finalizers: Vec::new(),
                },
                ConditionKind::Running,
                self.ready_timeout,
            )
            .await
            .map_err(|e| self.plane_error(&format!("start {}", request.container_name), e))?;
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
        tracing::info!(
            deployment_id = self.deployment_id,
            workload_id = view.id,
            container_id = %handle.id,
            "deployment replica started through the workload plane"
        );
        Ok(DeployResult {
            container_id: handle.id,
            container_name: handle.name,
            container_port,
            host_port,
            status: ContainerStatus::Running,
            docker_socket_mounted: false,
        })
    }

    async fn exec_in_container(
        &self,
        container_id: &str,
        command: Vec<String>,
        timeout: Duration,
    ) -> Result<temps_deployer::ExecResult, DeployerError> {
        self.local
            .exec_in_container(container_id, command, timeout)
            .await
    }

    async fn start_container(&self, container_id: &str) -> Result<(), DeployerError> {
        self.local.start_container(container_id).await
    }

    async fn stop_container(&self, container_id: &str) -> Result<(), DeployerError> {
        self.local.stop_container(container_id).await
    }

    async fn pause_container(&self, container_id: &str) -> Result<(), DeployerError> {
        self.local.pause_container(container_id).await
    }

    async fn resume_container(&self, container_id: &str) -> Result<(), DeployerError> {
        self.local.resume_container(container_id).await
    }

    /// Retire through the plane when this deployer started it (the plane
    /// removes the instance), else remove it directly.
    async fn remove_container(&self, container_id: &str) -> Result<(), DeployerError> {
        let workload = self
            .workloads
            .lock()
            .ok()
            .and_then(|mut workloads| workloads.remove(container_id));
        if let Some(workload_id) = workload {
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
                .map_err(|e| self.plane_error("retire the replica", e))?;
        }
        self.local.remove_container(container_id).await
    }

    async fn get_container_info(&self, container_id: &str) -> Result<ContainerInfo, DeployerError> {
        self.local.get_container_info(container_id).await
    }

    async fn get_container_stats(
        &self,
        container_id: &str,
    ) -> Result<temps_deployer::ContainerStats, DeployerError> {
        self.local.get_container_stats(container_id).await
    }

    async fn list_containers(&self) -> Result<Vec<ContainerInfo>, DeployerError> {
        self.local.list_containers().await
    }

    async fn get_container_logs(&self, container_id: &str) -> Result<String, DeployerError> {
        self.local.get_container_logs(container_id).await
    }

    async fn stream_container_logs(
        &self,
        container_id: &str,
    ) -> Result<Box<dyn futures::Stream<Item = String> + Unpin + Send>, DeployerError> {
        self.local.stream_container_logs(container_id).await
    }

    async fn image_exists(&self, image_name: &str) -> Result<bool, DeployerError> {
        self.local.image_exists(image_name).await
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
        let spec = deployment_workload_spec(&request(), 42, "staged:k".into()).unwrap();
        assert_eq!(spec.owner, WorkloadRef::new(WorkloadKind::Deployment, 42));
        assert_eq!(spec.placement.nodes, Some(vec![0]), "placed by the job");
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
    fn what_the_plane_cannot_deliver_is_refused_with_the_way_back() {
        let mut with_secret = request();
        with_secret
            .secrets
            .insert("DATABASE_PASSWORD".into(), "x".into());
        let error = deployment_workload_spec(&with_secret, 42, "staged:k".into()).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("app-production-42-1"), "{message}");
        assert!(message.contains("legacy"), "{message}");
        assert!(
            !message.contains("DATABASE_PASSWORD"),
            "never names a secret"
        );

        let mut with_socket = request();
        with_socket.control_plane_grants_socket = true;
        let message = deployment_workload_spec(&with_socket, 42, "staged:k".into())
            .unwrap_err()
            .to_string();
        assert!(
            message.contains("ADR 045") && message.contains("'shop'"),
            "{message}"
        );
    }
}
