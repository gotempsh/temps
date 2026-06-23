//! Container readiness probing.
//!
//! "Running" (Docker's container state) is not the same as "able to serve a
//! request": a container can be `Running` for some time before the process
//! inside binds its listening port. Two code paths care about the difference
//! and must not route/serve traffic before the app is actually accepting
//! connections:
//!
//! * **Scale-to-zero wake** (`temps-proxy`): the first request that wakes a
//!   sleeping environment is held until the container can serve it; otherwise
//!   that request races startup and gets a spurious upstream-connect 503.
//! * **Deployment completion** (`temps-deployments`): before flipping
//!   `current_deployment_id` to a new deployment we wait for its containers to
//!   accept connections, so we never flip routes onto a container that isn't
//!   serving yet (and fail the deployment if it never comes up).
//!
//! Both call into this module. The probe is a TCP connect to a published host
//! port on loopback — the same address the local node publishes container
//! ports on.
//!
//! **Scope:** the probe targets `127.0.0.1:{host_port}`. Containers running on
//! *remote* worker nodes are not reachable on this node's loopback (and a local
//! deployer can't `get_container_info` a remote container at all); multi-node
//! readiness is handled by the multi-node wake work tracked separately. For the
//! local single-node case this is correct. A container that publishes **no**
//! host port has nothing to probe, so the `Running` state is taken as ready.

use std::sync::Arc;
use std::time::{Duration, Instant};

use thiserror::Error;

use crate::{ContainerDeployer, ContainerStatus, DeployerError};

/// Why a readiness probe stopped before reporting the container ready.
#[derive(Error, Debug)]
pub enum ReadinessError {
    /// The container did not start accepting connections within the budget.
    #[error(
        "Container {container_id} did not accept connections within {timeout_secs}s \
         (last status: {last_status:?})"
    )]
    Timeout {
        container_id: String,
        timeout_secs: u64,
        /// The container's Docker status at the final poll, for diagnosis
        /// (e.g. `Exited` means it crashed on boot rather than booting slowly).
        last_status: Option<ContainerStatus>,
    },

    /// The container reached a terminal state (exited/dead) — it will never
    /// accept connections, so there is no point waiting out the timeout.
    #[error("Container {container_id} is in terminal state {status:?} and will not become ready")]
    Terminal {
        container_id: String,
        status: ContainerStatus,
    },

    /// Inspecting the container failed (the deployer errored).
    #[error("Failed to inspect container {container_id} while waiting for readiness: {source}")]
    Inspect {
        container_id: String,
        #[source]
        source: DeployerError,
    },
}

/// Tunables for [`wait_until_accepting_requests`]. `Default` matches the values
/// the scale-to-zero wake path historically used inline.
#[derive(Debug, Clone, Copy)]
pub struct ReadinessProbe {
    /// Total budget for the container to start accepting connections.
    pub timeout: Duration,
    /// Delay between successive readiness checks.
    pub poll_interval: Duration,
    /// Per-attempt TCP connect timeout. Kept short so a hung connect doesn't
    /// eat the whole poll interval.
    pub connect_timeout: Duration,
}

impl Default for ReadinessProbe {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            poll_interval: Duration::from_millis(500),
            connect_timeout: Duration::from_secs(2),
        }
    }
}

impl ReadinessProbe {
    /// Build a probe with the given overall timeout, keeping the default poll
    /// and connect intervals.
    pub fn with_timeout(timeout: Duration) -> Self {
        Self {
            timeout,
            ..Self::default()
        }
    }
}

/// Outcome of a single readiness check (see [`check_accepting_requests`]).
#[derive(Debug, Clone)]
pub enum ReadinessCheck {
    /// The container is `Running` and accepting connections (or has no port to
    /// probe). Safe to route/serve traffic.
    Ready,
    /// The container isn't ready yet but may still become ready — keep polling.
    /// Carries the observed status for logging.
    NotYet(ContainerStatus),
    /// The container is in a terminal state and will never accept connections.
    Terminal(ContainerStatus),
}

/// Perform a **single** readiness check: inspect the container, and if it is
/// `Running`, TCP-probe its lowest published host port.
///
/// This is the one-shot primitive shared by the scale-to-zero wake path (which
/// runs its own outer poll loop) and [`wait_until_accepting_requests`] (which
/// loops over this). The probe targets the **lowest** published host port
/// deterministically — Docker reports ports as an unordered map, so picking
/// `.first()` would be unstable for a container that publishes more than one.
/// A container with no published port is `Ready` once `Running` (nothing to
/// probe).
pub async fn check_accepting_requests(
    deployer: &Arc<dyn ContainerDeployer>,
    container_id: &str,
    connect_timeout: Duration,
) -> Result<ReadinessCheck, ReadinessError> {
    let info = deployer
        .get_container_info(container_id)
        .await
        .map_err(|e| ReadinessError::Inspect {
            container_id: container_id.to_string(),
            source: e,
        })?;

    Ok(match info.status {
        ContainerStatus::Running => match info.ports.iter().map(|p| p.host_port).min() {
            // No published port → nothing to probe; trust Running.
            None => ReadinessCheck::Ready,
            Some(port) => {
                if probe_port(port, connect_timeout).await {
                    ReadinessCheck::Ready
                } else {
                    ReadinessCheck::NotYet(ContainerStatus::Running)
                }
            }
        },
        // Terminal: the container will never accept connections.
        status @ (ContainerStatus::Exited | ContainerStatus::Dead) => {
            ReadinessCheck::Terminal(status)
        }
        // Not ready yet but not terminal — e.g. start_container is still
        // taking effect.
        status
        @ (ContainerStatus::Created | ContainerStatus::Paused | ContainerStatus::Stopped) => {
            ReadinessCheck::NotYet(status)
        }
    })
}

/// Wait until `container_id` is accepting TCP connections on a published host
/// port, polling [`check_accepting_requests`] until ready or the probe budget
/// is exhausted.
///
/// Returns `Ok(())` as soon as a connection succeeds. A container that
/// publishes no host port is considered ready once it is `Running` (there is
/// nothing to probe). A container that reaches a terminal state
/// (`Exited`/`Dead`) fails fast with [`ReadinessError::Terminal`] rather than
/// waiting out the full timeout — a crash-on-boot shouldn't cost the caller the
/// whole budget.
pub async fn wait_until_accepting_requests(
    deployer: &Arc<dyn ContainerDeployer>,
    container_id: &str,
    probe: ReadinessProbe,
) -> Result<(), ReadinessError> {
    let start = Instant::now();

    loop {
        // The status observed this iteration — surfaced in the timeout error so
        // a stuck deploy reports *why* (e.g. still `Created`, or `Running` but
        // not yet bound).
        let last_status =
            match check_accepting_requests(deployer, container_id, probe.connect_timeout).await? {
                ReadinessCheck::Ready => return Ok(()),
                ReadinessCheck::Terminal(status) => {
                    return Err(ReadinessError::Terminal {
                        container_id: container_id.to_string(),
                        status,
                    });
                }
                ReadinessCheck::NotYet(status) => status,
            };

        if start.elapsed() >= probe.timeout {
            return Err(ReadinessError::Timeout {
                container_id: container_id.to_string(),
                timeout_secs: probe.timeout.as_secs(),
                last_status: Some(last_status),
            });
        }

        tokio::time::sleep(probe.poll_interval).await;
    }
}

/// TCP-connect to `127.0.0.1:{port}` with a bounded timeout. A successful
/// handshake means the app inside has bound the port and can serve. A
/// refused/timed-out connection means it hasn't bound yet — reported as
/// not-ready so the caller keeps polling.
async fn probe_port(port: u16, connect_timeout: Duration) -> bool {
    let addr = format!("127.0.0.1:{}", port);
    match tokio::time::timeout(connect_timeout, tokio::net::TcpStream::connect(&addr)).await {
        Ok(Ok(_stream)) => true,
        Ok(Err(e)) => {
            tracing::debug!(
                addr = %addr,
                error = %e,
                "Readiness probe connect failed; container not ready yet"
            );
            false
        }
        Err(_) => {
            tracing::debug!(
                addr = %addr,
                "Readiness probe timed out; container not ready yet"
            );
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ContainerInfo, ContainerStats, DeployRequest, DeployResult, PortMapping, Protocol,
    };
    use async_trait::async_trait;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// Deployer that returns a queue of canned `ContainerInfo`s, one per
    /// `get_container_info` call (repeating the last once drained). Lets a test
    /// model a container that is `Created` for a poll or two and then `Running`.
    struct ScriptedDeployer {
        infos: Mutex<Vec<ContainerInfo>>,
    }

    impl ScriptedDeployer {
        #[allow(clippy::new_ret_no_self)] // intentionally returns the boxed trait object for tests
        fn new(infos: Vec<ContainerInfo>) -> Arc<dyn ContainerDeployer> {
            Arc::new(Self {
                infos: Mutex::new(infos),
            })
        }
    }

    fn info(status: ContainerStatus, ports: Vec<u16>) -> ContainerInfo {
        ContainerInfo {
            container_id: "c1".to_string(),
            container_name: "app".to_string(),
            image_name: "app:latest".to_string(),
            status,
            created_at: chrono::Utc::now(),
            ports: ports
                .into_iter()
                .map(|host_port| PortMapping {
                    host_port,
                    container_port: 3000,
                    protocol: Protocol::Tcp,
                })
                .collect(),
            environment_vars: HashMap::new(),
            restart_count: None,
            labels: HashMap::new(),
            exit_code: None,
            exit_reason: None,
            oom_killed: None,
            error_message: None,
            finished_at: None,
            started_at: None,
            cpu_limit_cores: None,
        }
    }

    #[async_trait]
    impl ContainerDeployer for ScriptedDeployer {
        async fn deploy_container(
            &self,
            _request: DeployRequest,
        ) -> Result<DeployResult, DeployerError> {
            unimplemented!()
        }
        async fn start_container(&self, _id: &str) -> Result<(), DeployerError> {
            unimplemented!()
        }
        async fn stop_container(&self, _id: &str) -> Result<(), DeployerError> {
            unimplemented!()
        }
        async fn pause_container(&self, _id: &str) -> Result<(), DeployerError> {
            unimplemented!()
        }
        async fn resume_container(&self, _id: &str) -> Result<(), DeployerError> {
            unimplemented!()
        }
        async fn remove_container(&self, _id: &str) -> Result<(), DeployerError> {
            unimplemented!()
        }
        async fn get_container_info(&self, _id: &str) -> Result<ContainerInfo, DeployerError> {
            let mut infos = self.infos.lock().unwrap();
            if infos.len() > 1 {
                Ok(infos.remove(0))
            } else {
                Ok(infos
                    .first()
                    .cloned()
                    .unwrap_or_else(|| info(ContainerStatus::Created, vec![])))
            }
        }
        async fn get_container_stats(&self, _id: &str) -> Result<ContainerStats, DeployerError> {
            unimplemented!()
        }
        async fn list_containers(&self) -> Result<Vec<ContainerInfo>, DeployerError> {
            unimplemented!()
        }
        async fn get_container_logs(&self, _id: &str) -> Result<String, DeployerError> {
            unimplemented!()
        }
        async fn stream_container_logs(
            &self,
            _id: &str,
        ) -> Result<Box<dyn futures::Stream<Item = String> + Unpin + Send>, DeployerError> {
            unimplemented!()
        }
    }

    fn fast_probe() -> ReadinessProbe {
        ReadinessProbe {
            timeout: Duration::from_secs(2),
            poll_interval: Duration::from_millis(10),
            connect_timeout: Duration::from_millis(200),
        }
    }

    #[tokio::test]
    async fn running_no_ports_is_ready() {
        let deployer = ScriptedDeployer::new(vec![info(ContainerStatus::Running, vec![])]);
        assert!(wait_until_accepting_requests(&deployer, "c1", fast_probe())
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn running_listening_port_is_ready() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let deployer = ScriptedDeployer::new(vec![info(ContainerStatus::Running, vec![port])]);
        assert!(wait_until_accepting_requests(&deployer, "c1", fast_probe())
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn closed_port_times_out() {
        // Bind then drop so the port is closed → connect refused forever.
        let port = {
            let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            l.local_addr().unwrap().port()
        };
        let deployer = ScriptedDeployer::new(vec![info(ContainerStatus::Running, vec![port])]);
        let err = wait_until_accepting_requests(&deployer, "c1", fast_probe())
            .await
            .unwrap_err();
        assert!(matches!(err, ReadinessError::Timeout { .. }), "got {err:?}");
    }

    #[tokio::test]
    async fn exited_fails_fast() {
        let deployer = ScriptedDeployer::new(vec![info(ContainerStatus::Exited, vec![])]);
        let err = wait_until_accepting_requests(&deployer, "c1", fast_probe())
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                ReadinessError::Terminal {
                    status: ContainerStatus::Exited,
                    ..
                }
            ),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn becomes_ready_after_a_few_polls() {
        // Created twice, then Running with a listening port.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let deployer = ScriptedDeployer::new(vec![
            info(ContainerStatus::Created, vec![]),
            info(ContainerStatus::Created, vec![]),
            info(ContainerStatus::Running, vec![port]),
        ]);
        assert!(wait_until_accepting_requests(&deployer, "c1", fast_probe())
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn probes_lowest_port_deterministically() {
        // Keep only the lower-numbered port listening; report high-then-low.
        let a = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let b = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let pa = a.local_addr().unwrap().port();
        let pb = b.local_addr().unwrap().port();
        let (lo, hi, lo_l, hi_l) = if pa < pb {
            (pa, pb, a, b)
        } else {
            (pb, pa, b, a)
        };
        drop(hi_l);
        let _keep = lo_l;
        let deployer = ScriptedDeployer::new(vec![info(ContainerStatus::Running, vec![hi, lo])]);
        assert!(
            wait_until_accepting_requests(&deployer, "c1", fast_probe())
                .await
                .is_ok(),
            "probe must target the lowest published port (the listening one)"
        );
    }
}
