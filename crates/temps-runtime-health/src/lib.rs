// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Node-local runtime probe primitives shared by embedded and worker hosts.
//!
//! This library has no database, Docker, HTTP API or exec dependency. The host
//! persists authorization fences, never restores execution leases after a
//! restart, and requires node-issued admission windows and reporting epochs.
//! Runtime adapters bind those assignments to host-owned creation receipts.

pub mod host;
mod probe;
mod scheduler;

pub use scheduler::ProbeEngine;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::{net::SocketAddr, time::Duration};
use thiserror::Error;
use uuid::Uuid;

/// Limits are implementation bounds, independent of any future API settings.
pub const MAX_ASSIGNMENTS: usize = 1_000;
pub const MAX_CONCURRENT_PROBES: usize = 4;
pub const MAX_LEASE: Duration = Duration::from_secs(120);
pub const MAX_HTTP_PATH_BYTES: usize = 1_024;

/// Opaque execution-role identity; physical co-location grants no authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(tag = "role", content = "id", rename_all = "snake_case")]
pub enum ExecutionOwner {
    Embedded(Uuid),
    Agent(Uuid),
}

/// Only read-only probe mechanisms are representable. In particular, there is
/// no command, user, mount, address or generic exec action in this contract.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Probe {
    Process { stable_for_secs: u64 },
    Http { path: String, expected_status: u16 },
    Tcp {},
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Assignment {
    pub id: Uuid,
    pub owner: ExecutionOwner,
    pub project_id: u32,
    pub environment_id: u32,
    pub deployment_id: u32,
    pub replica_id: u32,
    pub placement_generation: u64,
    pub revision: Uuid,
    pub epoch: u64,
    /// Full immutable runtime ID, never a name or Docker ID prefix.
    pub container_id: String,
    /// Container-side port. The trusted inspector derives the network endpoint.
    pub container_port: Option<u16>,
    pub probe: Probe,
    pub interval_secs: u64,
    pub timeout_secs: u64,
}

impl Assignment {
    fn validate(&self) -> Result<(), EngineError> {
        let owner_id = match self.owner {
            ExecutionOwner::Embedded(id) | ExecutionOwner::Agent(id) => id,
        };
        let valid_identity = !self.id.is_nil()
            && !self.revision.is_nil()
            && !owner_id.is_nil()
            && self.project_id > 0
            && self.environment_id > 0
            && self.deployment_id > 0
            && self.replica_id > 0
            && self.placement_generation > 0
            && self.epoch > 0
            && self.container_id.len() == 64
            && self.container_id.bytes().all(|b| b.is_ascii_hexdigit());
        let valid_timing =
            (1..=3_600).contains(&self.interval_secs) && (1..=30).contains(&self.timeout_secs);
        let valid_probe = match &self.probe {
            Probe::Process { stable_for_secs } => (1..=3_600).contains(stable_for_secs),
            Probe::Http {
                path,
                expected_status,
            } => {
                self.container_port.is_some_and(|port| port != 0)
                    && (200..300).contains(expected_status)
                    && path.starts_with('/')
                    && !path.starts_with("//")
                    && path.len() <= MAX_HTTP_PATH_BYTES
                    && path
                        .bytes()
                        .all(|b| b.is_ascii_graphic() && !matches!(b, b'\\' | b'#'))
            }
            Probe::Tcp {} => self.container_port.is_some_and(|port| port != 0),
        };
        if !valid_identity || !valid_timing || !valid_probe {
            return Err(EngineError::InvalidAssignment {
                assignment_id: self.id,
            });
        }
        Ok(())
    }

    fn freshness(&self) -> Duration {
        Duration::from_secs(self.interval_secs * 3 + self.timeout_secs)
    }
}

/// A read-only snapshot supplied by a trusted node-local runtime adapter.
/// It must verify the assignment against its host-owned deployment registry.
/// Labels or caller-selected endpoints alone are not sufficient authorization.
#[derive(Clone, Debug)]
pub struct RuntimeSnapshot {
    pub container_id: String,
    pub running: bool,
    pub restart_count: u64,
    /// Changes whenever the main process starts, including after daemon restart.
    pub started_at: String,
    /// Derived from runtime inspection of this container and its assigned port.
    /// This must never come from an API client's requested address.
    pub endpoint: Option<SocketAddr>,
}

/// Deliberately excludes stop/start/restart and exec, including from cleanup.
pub trait ProbePermit: Send + Sync {}
impl<T: Send + Sync> ProbePermit for T {}

#[async_trait]
pub trait RuntimeInspector: Send + Sync {
    /// Host admission holds this exclusive container claim for the entire lease.
    fn claim(&self, _assignment: &Assignment) -> Result<Box<dyn ProbePermit>, InspectFailure> {
        Ok(Box::new(()))
    }

    /// Held through inspection and the network probe, including cancellation.
    /// Production adapters use host-wide slots shared by co-located processes.
    async fn admit(
        &self,
        _assignment: &Assignment,
    ) -> Result<Box<dyn ProbePermit>, InspectFailure> {
        Ok(Box::new(()))
    }
    async fn inspect(&self, assignment: &Assignment) -> Result<RuntimeSnapshot, InspectFailure>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum InspectFailure {
    #[error("assigned runtime is unavailable")]
    Unavailable,
    #[error("container assignment is not authorized")]
    Unauthorized,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum HealthState {
    Healthy,
    Unhealthy,
    Unknown,
}

/// Allowlisted diagnostics only; raw runtime or application output cannot fit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum HealthReason {
    Passed,
    Stabilizing,
    NotRunning,
    Restarted,
    RuntimeUnavailable,
    Unauthorized,
    IdentityMismatch,
    EndpointUnavailable,
    ConnectionFailed,
    ProbeTimeout,
    InvalidHttpResponse,
    HttpStatusMismatch,
    LeaseExpired,
    Cancelled,
    Stale,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Observation {
    pub assignment_id: Uuid,
    pub revision: Uuid,
    pub placement_generation: u64,
    pub epoch: u64,
    pub sequence: u64,
    pub state: HealthState,
    pub reason: HealthReason,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum EngineError {
    #[error(
        "Runtime health assignment {assignment_id} has invalid identity, timing or probe settings"
    )]
    InvalidAssignment { assignment_id: Uuid },
    #[error("Runtime health assignment {assignment_id} targets a different execution owner")]
    WrongOwner { assignment_id: Uuid },
    #[error(
        "Runtime health assignment {assignment_id} has an expired or excessive execution lease"
    )]
    InvalidLease { assignment_id: Uuid },
    #[error("Runtime health assignment {assignment_id} is already installed; renew or remove it explicitly")]
    AlreadyInstalled { assignment_id: Uuid },
    #[error("Runtime health container already has assignment {assignment_id}")]
    ContainerOwned { assignment_id: Uuid },
    #[error("Runtime health assignment {assignment_id} is not installed at epoch {epoch}")]
    UnknownAssignment { assignment_id: Uuid, epoch: u64 },
    #[error("Runtime health assignment {assignment_id} still owns an in-flight probe")]
    Busy { assignment_id: Uuid },
    #[error("Runtime health engine reached its {limit}-assignment bound")]
    Capacity { limit: usize },
}

#[cfg(test)]
mod docker_tests {
    use super::*;
    use bollard::{
        query_parameters::{InspectContainerOptions, StopContainerOptions},
        Docker,
    };
    use std::{
        net::{IpAddr, Ipv4Addr},
        sync::Arc,
    };
    use testcontainers::{
        core::{IntoContainerPort, WaitFor},
        runners::AsyncRunner,
        GenericImage, ImageExt,
    };
    use tokio::time::Instant;

    /// A fixture adapter authorized only for the immutable container just created
    /// by this test. Production adapters/registry persistence are separate work.
    struct OwnedDockerFixture {
        docker: Docker,
        container_id: String,
        endpoint: SocketAddr,
    }

    #[async_trait]
    impl RuntimeInspector for OwnedDockerFixture {
        async fn inspect(
            &self,
            assignment: &Assignment,
        ) -> Result<RuntimeSnapshot, InspectFailure> {
            if assignment.container_id != self.container_id {
                return Err(InspectFailure::Unauthorized);
            }
            let info = self
                .docker
                .inspect_container(&self.container_id, None::<InspectContainerOptions>)
                .await
                .map_err(|_| InspectFailure::Unavailable)?;
            let state = info.state.ok_or(InspectFailure::Unavailable)?;
            Ok(RuntimeSnapshot {
                container_id: info.id.ok_or(InspectFailure::Unavailable)?,
                running: state.running == Some(true),
                restart_count: info
                    .restart_count
                    .unwrap_or_default()
                    .try_into()
                    .map_err(|_| InspectFailure::Unavailable)?,
                started_at: state.started_at.ok_or(InspectFailure::Unavailable)?,
                endpoint: assignment.container_port.map(|_| self.endpoint),
            })
        }
    }

    #[tokio::test]
    async fn runtime_probes_real_docker_worker_process_http_tcp_and_stopped_state() {
        let Ok(docker) = Docker::connect_with_defaults() else {
            eprintln!("Docker unavailable; skipping runtime probe integration");
            return;
        };
        if docker.ping().await.is_err() {
            eprintln!("Docker unavailable; skipping runtime probe integration");
            return;
        }
        // Separate Docker VMs can allocate a published port already occupied
        // on the host. Select a host-free port for this owned fixture.
        let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let requested_port = listener.local_addr().unwrap().port();
        drop(listener);
        let fixture = GenericImage::new("busybox", "1.37.0")
            .with_entrypoint("sh")
            .with_exposed_port(8080.tcp())
            .with_wait_for(WaitFor::message_on_stdout("runtime-probe-fixture-ready"))
            .with_mapped_port(requested_port, 8080.tcp())
            .with_container_name(format!("temps-runtime-probe-{}", Uuid::new_v4()))
            .with_cmd(["-c", "mkdir -p /tmp/site; printf ready > /tmp/site/index.html; httpd -f -p 8080 -h /tmp/site & echo runtime-probe-fixture-ready; exec sleep 120"])
            .start().await.unwrap();
        let container_id = fixture.id().to_string();
        let host = fixture
            .get_host()
            .await
            .unwrap()
            .to_string()
            .parse()
            .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST));
        let endpoint = SocketAddr::new(host, fixture.get_host_port_ipv4(8080).await.unwrap());
        let runtime = Arc::new(OwnedDockerFixture {
            docker: docker.clone(),
            container_id: container_id.clone(),
            endpoint,
        });
        let owner = ExecutionOwner::Embedded(Uuid::new_v4());
        let engine = ProbeEngine::new(owner, runtime);
        let mut assignment = Assignment {
            id: Uuid::new_v4(),
            owner,
            project_id: 1,
            environment_id: 1,
            deployment_id: 1,
            replica_id: 1,
            placement_generation: 1,
            revision: Uuid::new_v4(),
            epoch: 1,
            container_id: container_id.clone(),
            container_port: None,
            probe: Probe::Process { stable_for_secs: 1 },
            interval_secs: 1,
            timeout_secs: 5,
        };
        engine
            .install(assignment.clone(), Instant::now() + MAX_LEASE)
            .unwrap();
        assert_eq!(engine.run_due().await[0].reason, HealthReason::Stabilizing);
        tokio::time::sleep(Duration::from_millis(1_050)).await;
        assert_eq!(engine.run_due().await[0].state, HealthState::Healthy);
        engine.remove(assignment.id, 1).unwrap();

        for (probe, state) in [
            (
                Probe::Http {
                    path: "/".into(),
                    expected_status: 200,
                },
                HealthState::Healthy,
            ),
            (
                Probe::Http {
                    path: "/missing".into(),
                    expected_status: 200,
                },
                HealthState::Unhealthy,
            ),
            (Probe::Tcp {}, HealthState::Healthy),
        ] {
            assignment.id = Uuid::new_v4();
            assignment.revision = Uuid::new_v4();
            assignment.container_port = Some(8080);
            assignment.probe = probe;
            engine
                .install(assignment.clone(), Instant::now() + MAX_LEASE)
                .unwrap();
            let observation = engine.run_due().await.remove(0);
            assert_eq!(observation.state, state, "{observation:?}");
            engine.remove(assignment.id, 1).unwrap();
        }

        let running = docker
            .inspect_container(&container_id, None::<InspectContainerOptions>)
            .await
            .unwrap();
        assert_eq!(running.state.unwrap().running, Some(true));
        assert_eq!(running.restart_count, Some(0));
        // Only the fixture driver changes lifecycle; the engine has no such API.
        docker
            .stop_container(
                &container_id,
                Some(StopContainerOptions {
                    t: Some(1),
                    signal: None,
                }),
            )
            .await
            .unwrap();
        assignment.id = Uuid::new_v4();
        engine
            .install(assignment, Instant::now() + MAX_LEASE)
            .unwrap();
        let report = engine.run_due().await.remove(0);
        assert_eq!(
            (report.state, report.reason),
            (HealthState::Unhealthy, HealthReason::NotRunning)
        );
        // Testcontainers removes only this uniquely named owned fixture on drop.
    }
}
