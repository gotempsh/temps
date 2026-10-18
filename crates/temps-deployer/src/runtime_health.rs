// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Read-only Docker adapter and host-owned creation receipts (ADR-053).
//! Receipts are written from trusted deploy operations, never discovered from
//! inspect labels. Existing containers need an authorized redeployment.

use crate::DeployRequest;
use async_trait::async_trait;
use bollard::{query_parameters::InspectContainerOptions, Docker};
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use temps_runtime_health::{
    host::{persist, private_directory, private_file, read_record, HostError, ProbeHost},
    Assignment, ExecutionOwner, InspectFailure, ProbePermit, RuntimeInspector, RuntimeSnapshot,
    MAX_CONCURRENT_PROBES,
};
use uuid::Uuid;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreationReceipt {
    container_id: String,
    project_id: u32,
    environment_id: u32,
    deployment_id: u32,
    image_id: String,
    ports: Vec<u16>,
}

pub fn data_root() -> PathBuf {
    std::env::var_os("TEMPS_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".temps")
        })
        .join("runtime-health")
}

fn valid_container_id(id: &str) -> bool {
    id.len() == 64 && id.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Called immediately after Docker creates an application container. IDs here
/// come from the authorized deployment request, not workload-controlled labels
/// later read back from Docker.
pub async fn record_creation(
    docker: &Docker,
    root: &Path,
    id: &str,
    request: &DeployRequest,
) -> Result<(), HostError> {
    let ids = [
        "sh.temps.project_id",
        "sh.temps.environment",
        "sh.temps.deploy_id",
    ]
    .map(|key| {
        request
            .labels
            .get(key)
            .and_then(|value| value.parse::<u32>().ok())
            .filter(|id| *id > 0)
    });
    let [Some(project_id), Some(environment_id), Some(deployment_id)] = ids else {
        return Ok(());
    };
    if !valid_container_id(id) {
        return Err(HostError::Fenced);
    }
    let info = docker
        .inspect_container(id, None::<InspectContainerOptions>)
        .await
        .map_err(|_| HostError::Fenced)?;
    let receipt = CreationReceipt {
        container_id: id.into(),
        project_id,
        environment_id,
        deployment_id,
        image_id: info
            .image
            .filter(|id| !id.is_empty())
            .ok_or(HostError::Fenced)?,
        ports: request
            .port_mappings
            .iter()
            .map(|mapping| mapping.container_port)
            .collect(),
    };
    let root = root.join("receipts");
    let name = format!("{id}.json");
    tokio::task::spawn_blocking(move || {
        private_directory(&root)?;
        persist(&root, &name, &receipt)
    })
    .await
    .map_err(|_| HostError::JournalFailed)?
}

/// Container removal revokes the creation receipt before discarding fences.
/// No live container can be reauthorized after its receipt has been removed.
pub fn forget_creation(root: &Path, id: &str) {
    if !valid_container_id(id) {
        return;
    }
    for directory in ["receipts", "embedded", "agent"] {
        let path = root.join(directory).join(format!("{id}.json"));
        if let Err(error) = std::fs::remove_file(&path) {
            if error.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(container_id=id,%error,"Could not remove retired runtime health receipt");
            }
        }
    }
}

struct ContainerClaim {
    _container: File,
    _role: Arc<File>,
}

pub struct DockerProbeInspector {
    docker: Arc<Docker>,
    receipts: PathBuf,
    locks: PathBuf,
    role_claim: std::sync::Mutex<(Option<ExecutionOwner>, std::sync::Weak<File>)>,
}

impl DockerProbeInspector {
    pub async fn new(docker: Arc<Docker>, root: &Path) -> Result<Self, HostError> {
        Self::with_coordination(
            docker,
            root,
            crate::runtime_health_coordination::COORDINATION_VOLUME,
        )
        .await
    }
    async fn with_coordination(
        docker: Arc<Docker>,
        root: &Path,
        coordination_volume: &str,
    ) -> Result<Self, HostError> {
        let info = tokio::time::timeout(Duration::from_secs(5), docker.info())
            .await
            .map_err(|_| HostError::Fenced)?
            .map_err(|_| HostError::Fenced)?;
        let daemon = info
            .id
            .filter(|id| !id.is_empty() && id.len() <= 128)
            .ok_or(HostError::Fenced)?;
        let locks = crate::runtime_health_coordination::namespace(
            &docker,
            root,
            &daemon,
            coordination_volume,
        )
        .await?;
        let receipts = root.join("receipts");
        private_directory(&receipts)?;
        Ok(Self {
            docker,
            receipts,
            locks,
            role_claim: std::sync::Mutex::new((None, std::sync::Weak::new())),
        })
    }

    fn receipt(&self, assignment: &Assignment) -> Result<CreationReceipt, InspectFailure> {
        if !valid_container_id(&assignment.container_id) {
            return Err(InspectFailure::Unauthorized);
        }
        let receipt: CreationReceipt = read_record(
            &self
                .receipts
                .join(format!("{}.json", assignment.container_id)),
        )
        .map_err(|_| InspectFailure::Unauthorized)?
        .ok_or(InspectFailure::Unauthorized)?;
        if receipt.container_id != assignment.container_id
            || receipt.project_id != assignment.project_id
            || receipt.environment_id != assignment.environment_id
            || receipt.deployment_id != assignment.deployment_id
            || assignment
                .container_port
                .is_some_and(|port| !receipt.ports.contains(&port))
        {
            return Err(InspectFailure::Unauthorized);
        }
        Ok(receipt)
    }

    fn lock(&self, name: &str) -> Result<File, InspectFailure> {
        let file = private_file(&self.locks.join(name)).map_err(|_| InspectFailure::Unavailable)?;
        file.try_lock().map_err(|_| InspectFailure::Unavailable)?;
        Ok(file)
    }
}

#[async_trait]
impl RuntimeInspector for DockerProbeInspector {
    fn claim(&self, assignment: &Assignment) -> Result<Box<dyn ProbePermit>, InspectFailure> {
        self.receipt(assignment)?;
        let container = self.lock(&format!("{}.lock", assignment.container_id))?;
        let mut claim = self
            .role_claim
            .lock()
            .map_err(|_| InspectFailure::Unavailable)?;
        if claim.0.is_some_and(|owner| owner != assignment.owner) {
            return Err(InspectFailure::Unauthorized);
        }
        let role = match claim.1.upgrade() {
            Some(role) => role,
            None => {
                let role = match assignment.owner {
                    ExecutionOwner::Embedded(_) => "embedded",
                    ExecutionOwner::Agent(_) => "agent",
                };
                let role = Arc::new(self.lock(&format!("owner-{role}.lock"))?);
                *claim = (Some(assignment.owner), Arc::downgrade(&role));
                role
            }
        };
        Ok(Box::new(ContainerClaim {
            _container: container,
            _role: role,
        }))
    }

    async fn admit(&self, assignment: &Assignment) -> Result<Box<dyn ProbePermit>, InspectFailure> {
        self.receipt(assignment)?;
        // Four slots shared across every process accessing this daemon. These
        // guards survive through network I/O and drop on cancellation.
        let embedded = matches!(assignment.owner, ExecutionOwner::Embedded(_));
        let other_role = if embedded { "agent" } else { "embedded" };
        let other_active = self.lock(&format!("owner-{other_role}.lock")).is_err();
        // Each active role keeps two slots when co-located. A lone role can
        // borrow all four; handover still obeys the shared file locks.
        let slots = if other_active {
            if embedded {
                0..2
            } else {
                2..4
            }
        } else {
            0..MAX_CONCURRENT_PROBES
        };
        for slot in slots {
            if let Ok(file) = self.lock(&format!("slot-{slot}.lock")) {
                return Ok(Box::new(file));
            }
        }
        Err(InspectFailure::Unavailable)
    }

    async fn inspect(&self, assignment: &Assignment) -> Result<RuntimeSnapshot, InspectFailure> {
        let receipt = self.receipt(assignment)?;
        let info = self
            .docker
            .inspect_container(&assignment.container_id, None::<InspectContainerOptions>)
            .await
            .map_err(|_| InspectFailure::Unavailable)?;
        if info.id.as_deref() != Some(&assignment.container_id)
            || info.image.as_deref() != Some(&receipt.image_id)
        {
            return Err(InspectFailure::Unauthorized);
        }
        let state = info.state.ok_or(InspectFailure::Unavailable)?;
        // Only this container's published port is eligible. Do not accept an
        // address, hostname, redirect or unassigned exposed port from callers.
        let endpoint = assignment.container_port.and_then(|port| {
            info.network_settings
                .as_ref()?
                .ports
                .as_ref()?
                .get(&format!("{port}/tcp"))?
                .as_ref()?
                .iter()
                .find_map(|binding| {
                    let ip: IpAddr = binding.host_ip.as_ref()?.parse().ok()?;
                    let port: u16 = binding.host_port.as_ref()?.parse().ok()?;
                    let ip = if ip.is_unspecified() {
                        "127.0.0.1".parse().ok()?
                    } else {
                        ip
                    };
                    (port != 0 && !ip.is_multicast()).then_some(SocketAddr::new(ip, port))
                })
        });
        Ok(RuntimeSnapshot {
            container_id: assignment.container_id.clone(),
            running: state.running.unwrap_or(false),
            restart_count: info.restart_count.unwrap_or(0).max(0) as u64,
            started_at: state.started_at.unwrap_or_default(),
            endpoint,
        })
    }
}

pub async fn open_host(
    docker: Arc<Docker>,
    root: PathBuf,
    agent: bool,
) -> Result<Arc<ProbeHost>, HostError> {
    private_directory(&root)?;
    let identity_path = root.join("identity.json");
    // Serialization by identity.lock prevents concurrent first boot from
    // producing distinct execution identities for the same data directory.
    let identity_lock = private_file(&root.join("identity.lock"))?;
    identity_lock.try_lock().map_err(|_| HostError::Fenced)?;
    let identity = match read_record::<Uuid>(&identity_path)? {
        Some(identity) if !identity.is_nil() => identity,
        Some(_) => return Err(HostError::Fenced),
        None => {
            let identity = Uuid::new_v4();
            persist(&root, "identity.json", &identity)?;
            identity
        }
    };
    let inspector = Arc::new(DockerProbeInspector::new(docker, &root).await?);
    let owner = if agent {
        ExecutionOwner::Agent(identity)
    } else {
        ExecutionOwner::Embedded(identity)
    };
    let host = Arc::new(ProbeHost::open(
        owner,
        root.join(if agent { "agent" } else { "embedded" }),
        inspector,
    )?);
    Ok(host)
}

/// A single timer per host. In-flight checks can overlap timer ticks, but the
/// engine and daemon-wide permits enforce the fixed concurrency bound.
pub fn spawn_host(host: Arc<ProbeHost>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut timer = tokio::time::interval(Duration::from_millis(125));
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut pending = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                _ = timer.tick() => {
                    if pending.len() < MAX_CONCURRENT_PROBES {
                        let host = host.clone();
                        pending.spawn(async move { host.run_due().await });
                    }
                }
                Some(result) = pending.join_next(), if !pending.is_empty() => {
                    if let Err(error) = result { tracing::error!(%error, "Runtime health scheduler task failed"); }
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PortMapping, Protocol, ResourceLimits, RestartPolicy};
    use temps_runtime_health::Probe;
    use testcontainers::{
        core::{IntoContainerPort, WaitFor},
        runners::AsyncRunner,
        GenericImage, ImageExt,
    };

    #[tokio::test]
    async fn docker_receipts_and_co_located_locks_bind_identity_ports_and_shared_slots(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let Ok(docker) = Docker::connect_with_local_defaults() else {
            eprintln!("Docker unavailable; skipping receipt/ownership integration");
            return Ok(());
        };
        if docker.ping().await.is_err() {
            eprintln!("Docker unavailable; skipping receipt/ownership integration");
            return Ok(());
        }
        let docker = Arc::new(docker);
        let container=GenericImage::new("busybox","1.37").with_exposed_port(8080.tcp()).with_wait_for(WaitFor::message_on_stdout("probe-receipt-ready")).with_entrypoint("sh").with_cmd(["-c","mkdir -p /tmp/site; printf ready > /tmp/site/index.html; httpd -f -p 8080 -h /tmp/site & echo probe-receipt-ready; exec sleep 120"]).start().await?;
        let root_a = tempfile::tempdir()?;
        let root_b = tempfile::tempdir()?;
        let coordination = format!("temps-runtime-health-test-{}", Uuid::new_v4());
        let inspector_a =
            DockerProbeInspector::with_coordination(docker.clone(), root_a.path(), &coordination)
                .await?;
        let inspector_b =
            DockerProbeInspector::with_coordination(docker.clone(), root_b.path(), &coordination)
                .await?;
        assert_eq!(inspector_a.locks, inspector_b.locks);
        let mut assignment = Assignment {
            id: Uuid::new_v4(),
            owner: ExecutionOwner::Embedded(Uuid::new_v4()),
            project_id: 1,
            environment_id: 2,
            deployment_id: 3,
            replica_id: 4,
            placement_generation: 1,
            revision: Uuid::new_v4(),
            epoch: 1,
            container_id: container.id().into(),
            container_port: Some(8080),
            probe: Probe::Http {
                path: "/".into(),
                expected_status: 200,
            },
            interval_secs: 30,
            timeout_secs: 5,
        };
        assert!(matches!(
            inspector_a.inspect(&assignment).await,
            Err(InspectFailure::Unauthorized)
        ));
        let request = DeployRequest {
            image_name: "busybox:1.37".into(),
            container_name: "receipt-fixture".into(),
            environment_vars: Default::default(),
            secrets: Default::default(),
            port_mappings: vec![PortMapping {
                host_port: container.get_host_port_ipv4(8080).await?,
                container_port: 8080,
                protocol: Protocol::Tcp,
                host_ip: None,
            }],
            network_name: None,
            extra_networks: Vec::new(),
            resource_limits: ResourceLimits::default(),
            restart_policy: RestartPolicy::Never,
            log_path: root_a.path().join("fixture.log"),
            command: None,
            log_config: None,
            labels: [
                ("sh.temps.project_id".into(), "1".into()),
                ("sh.temps.environment".into(), "2".into()),
                ("sh.temps.deploy_id".into(), "3".into()),
            ]
            .into(),
            project_slug: None,
            control_plane_grants_socket: false,
        };
        record_creation(&docker, root_a.path(), container.id(), &request).await?;
        record_creation(&docker, root_b.path(), container.id(), &request).await?;
        let claim = inspector_a.claim(&assignment)?;
        assert!(
            inspector_b.claim(&assignment).is_err(),
            "a co-located owner cannot duplicate admission"
        );
        let snapshot = inspector_a.inspect(&assignment).await?;
        assert!(snapshot.running);
        assert_eq!(
            snapshot.endpoint.unwrap().port(),
            container.get_host_port_ipv4(8080).await?
        );
        assignment.project_id = 9;
        assert!(matches!(
            inspector_a.inspect(&assignment).await,
            Err(InspectFailure::Unauthorized)
        ));
        assignment.project_id = 1;
        assignment.container_port = Some(9090);
        assert!(matches!(
            inspector_a.inspect(&assignment).await,
            Err(InspectFailure::Unauthorized)
        ));
        assignment.container_port = Some(8080);
        let mut slots = Vec::new();
        for index in 0..MAX_CONCURRENT_PROBES {
            let inspector = if index % 2 == 0 {
                &inspector_a
            } else {
                &inspector_b
            };
            slots.push(inspector.admit(&assignment).await?);
        }
        assert!(
            inspector_b.admit(&assignment).await.is_err(),
            "the daemon-wide budget is shared between process roles"
        );
        slots.pop();
        assert!(inspector_b.admit(&assignment).await.is_ok());
        drop(claim);
        assert!(inspector_b.claim(&assignment).is_ok());
        let after = inspector_a.inspect(&assignment).await?;
        assert!(after.running);
        assert_eq!(after.restart_count, snapshot.restart_count);
        assert_eq!(after.started_at, snapshot.started_at);
        let embedded = inspector_a.claim(&assignment)?;
        let child = std::process::Command::new(std::env::current_exe()?)
            .arg("--exact")
            .arg("runtime_health::tests::cohost_lock_child")
            .env(
                "TEMPS_RUNTIME_HEALTH_TEST_LOCK",
                inspector_a
                    .locks
                    .join(format!("{}.lock", assignment.container_id)),
            )
            .output()?;
        assert!(
            child.status.success(),
            "a separate OS process must see the container claim"
        );
        drop(slots);
        let second = GenericImage::new("busybox", "1.37")
            .with_entrypoint("sleep")
            .with_cmd(["120"])
            .start()
            .await?;
        let mut second_request = request.clone();
        second_request.port_mappings.clear();
        record_creation(&docker, root_b.path(), second.id(), &second_request).await?;
        let agent =
            DockerProbeInspector::with_coordination(docker.clone(), root_b.path(), &coordination)
                .await?;
        let mut second_assignment = assignment.clone();
        second_assignment.id = Uuid::new_v4();
        second_assignment.owner = ExecutionOwner::Agent(Uuid::new_v4());
        second_assignment.container_id = second.id().into();
        second_assignment.container_port = None;
        second_assignment.probe = Probe::Process { stable_for_secs: 1 };
        let agent_claim = agent.claim(&second_assignment)?;
        let mut shared = Vec::new();
        for _ in 0..2 {
            shared.push(inspector_a.admit(&assignment).await?);
        }
        assert!(
            inspector_a.admit(&assignment).await.is_err(),
            "the embedded role leaves capacity for its co-located agent"
        );
        for _ in 0..2 {
            shared.push(agent.admit(&second_assignment).await?);
        }
        assert!(agent.admit(&second_assignment).await.is_err());
        drop(shared);
        drop(agent_claim);
        drop(embedded);
        docker
            .remove_volume(
                &coordination,
                None::<bollard::query_parameters::RemoveVolumeOptions>,
            )
            .await?;
        Ok(())
    }
    #[test]
    fn cohost_lock_child() {
        let Some(path) = std::env::var_os("TEMPS_RUNTIME_HEALTH_TEST_LOCK") else {
            return;
        };
        let file = private_file(Path::new(&path)).unwrap();
        assert!(
            file.try_lock().is_err(),
            "a live claim must be exclusive across processes"
        );
    }
}
