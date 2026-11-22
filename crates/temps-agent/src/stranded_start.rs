// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Start the containers Docker could not start because the mesh address they
//! publish on did not exist yet.
//!
//! At boot Docker starts every container with a restart policy before
//! `temps-agent` (which runs after `docker.service`) has created
//! `temps-wg0`. A container that publishes a port on this node's mesh
//! address then fails to start with
//!
//! ```text
//! failed to bind host port 10.201.0.8:6263/tcp: cannot assign requested address
//! ```
//!
//! and Docker never retries it: restart policies apply to a container whose
//! process exited, not to a start that failed. Only the agent knows when the
//! address exists, so once it has brought the mesh interface up it starts
//! exactly those containers again.
//!
//! A container is started only when all of these hold, so nothing the
//! operator stopped, and nothing that failed for another reason, is touched:
//!
//! - it is not running (`created` or `exited`);
//! - its restart policy asks Docker to keep it running (`always`,
//!   `unless-stopped` or `on-failure`): this restores what Docker would have
//!   done had the address existed;
//! - its last start failed with "cannot assign requested address";
//! - it publishes at least one port on the mesh address.

use std::collections::HashMap;
use std::net::Ipv4Addr;

use async_trait::async_trait;
use bollard::models::{ContainerInspectResponse, ContainerStateStatusEnum, RestartPolicyNameEnum};
use bollard::Docker;
use tracing::{info, warn};

/// The kernel's `EADDRNOTAVAIL` text, as Docker reports a published port
/// whose host address is not assigned to any interface.
const ADDRESS_NOT_AVAILABLE: &str = "cannot assign requested address";

/// A container whose last start failed because the mesh address was missing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StrandedContainer {
    pub container_id: String,
    pub container_name: String,
    /// `sh.temps.deploy_id`, for app deployments.
    pub deployment_id: Option<String>,
    /// `sh.temps.service.name`, for services created through the agent API.
    pub service_name: Option<String>,
    /// Docker's error from the failed start.
    pub start_error: String,
}

/// Decide from `docker inspect` whether a container is stranded by the
/// missing mesh address `address`; `None` when it must be left alone.
pub(crate) fn stranded_on(
    inspect: &ContainerInspectResponse,
    address: Ipv4Addr,
) -> Option<StrandedContainer> {
    let state = inspect.state.as_ref()?;
    if state.running == Some(true)
        || !matches!(
            state.status,
            Some(ContainerStateStatusEnum::CREATED | ContainerStateStatusEnum::EXITED)
        )
    {
        return None;
    }
    let start_error = state.error.as_deref().map(str::trim).unwrap_or_default();
    if !start_error
        .to_ascii_lowercase()
        .contains(ADDRESS_NOT_AVAILABLE)
    {
        return None;
    }

    let host_config = inspect.host_config.as_ref()?;
    let keep_running = host_config
        .restart_policy
        .as_ref()
        .and_then(|policy| policy.name.as_ref())
        .is_some_and(|name| match name {
            RestartPolicyNameEnum::ALWAYS
            | RestartPolicyNameEnum::UNLESS_STOPPED
            | RestartPolicyNameEnum::ON_FAILURE => true,
            RestartPolicyNameEnum::NO | RestartPolicyNameEnum::EMPTY => false,
        });
    if !keep_running {
        return None;
    }

    let address = address.to_string();
    let publishes_on_mesh = host_config
        .port_bindings
        .iter()
        .flat_map(HashMap::values)
        .flatten()
        .flatten()
        .any(|binding| binding.host_ip.as_deref() == Some(address.as_str()));
    if !publishes_on_mesh {
        return None;
    }

    let labels = inspect
        .config
        .as_ref()
        .and_then(|config| config.labels.as_ref());
    Some(StrandedContainer {
        container_id: inspect.id.clone().unwrap_or_default(),
        container_name: inspect
            .name
            .as_deref()
            .unwrap_or_default()
            .trim_start_matches('/')
            .to_string(),
        deployment_id: labels.and_then(|labels| labels.get("sh.temps.deploy_id").cloned()),
        service_name: labels.and_then(|labels| labels.get("sh.temps.service.name").cloned()),
        start_error: start_error.to_string(),
    })
}

/// The Docker calls [`start_stranded_containers`] needs, so the selection and
/// start logic is testable without a daemon.
#[async_trait]
pub(crate) trait ContainerEngine: Send + Sync {
    /// IDs of every container that is not running (`created` or `exited`).
    async fn stopped_container_ids(&self) -> Result<Vec<String>, String>;
    async fn inspect(&self, container_id: &str) -> Result<ContainerInspectResponse, String>;
    async fn start(&self, container_id: &str) -> Result<(), String>;
}

#[async_trait]
impl ContainerEngine for Docker {
    async fn stopped_container_ids(&self) -> Result<Vec<String>, String> {
        use bollard::query_parameters::ListContainersOptions;

        let filters = HashMap::from([(
            "status".to_string(),
            vec!["created".to_string(), "exited".to_string()],
        )]);
        let listed = self
            .list_containers(Some(ListContainersOptions {
                all: true,
                filters: Some(filters),
                ..Default::default()
            }))
            .await
            .map_err(|e| format!("listing stopped containers: {e}"))?;
        Ok(listed
            .into_iter()
            .filter_map(|summary| summary.id)
            .collect())
    }

    async fn inspect(&self, container_id: &str) -> Result<ContainerInspectResponse, String> {
        use bollard::query_parameters::InspectContainerOptions;

        self.inspect_container(container_id, None::<InspectContainerOptions>)
            .await
            .map_err(|e| format!("inspecting container {container_id}: {e}"))
    }

    async fn start(&self, container_id: &str) -> Result<(), String> {
        use bollard::query_parameters::StartContainerOptions;

        self.start_container(container_id, None::<StartContainerOptions>)
            .await
            .map_err(|e| format!("starting container {container_id}: {e}"))
    }
}

/// Whether the sync loop should look for stranded containers on this tick:
/// once per mesh address this process brought up, and again whenever the
/// interface was rebuilt (anything Docker tried to start while it was gone
/// failed the same way).
pub(crate) fn should_check(
    checked_for: Option<Ipv4Addr>,
    current: Ipv4Addr,
    interface_rebuilt: bool,
) -> bool {
    interface_rebuilt || checked_for != Some(current)
}

/// What one pass of [`start_stranded_containers`] did.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct StartReport {
    /// Names of the containers started.
    pub started: Vec<String>,
    /// Containers that still failed to start, with Docker's error.
    pub failed: Vec<(String, String)>,
}

/// Start every container stranded by the mesh address `address` not existing
/// when Docker tried to start it. Called once the mesh interface is up.
///
/// Best effort: a container that cannot be inspected or started is logged
/// and skipped. Only failing to list containers at all is an error.
pub(crate) async fn start_stranded_containers(
    engine: &dyn ContainerEngine,
    address: Ipv4Addr,
) -> Result<StartReport, String> {
    let mut report = StartReport::default();
    for container_id in engine.stopped_container_ids().await? {
        let inspect = match engine.inspect(&container_id).await {
            Ok(inspect) => inspect,
            Err(error) => {
                // Removed between the list and the inspect, most likely.
                warn!(%container_id, %error, "skipping a stopped container that could not be inspected");
                continue;
            }
        };
        let Some(container) = stranded_on(&inspect, address) else {
            continue;
        };
        match engine.start(&container.container_id).await {
            Ok(()) => {
                info!(
                    container_id = %container.container_id,
                    container_name = %container.container_name,
                    deployment_id = container.deployment_id.as_deref().unwrap_or("-"),
                    service_name = container.service_name.as_deref().unwrap_or("-"),
                    mesh_address = %address,
                    previous_error = %container.start_error,
                    "started a container Docker could not start before this node's WireGuard \
                     mesh address existed"
                );
                report.started.push(container.container_name);
            }
            Err(error) => {
                warn!(
                    container_id = %container.container_id,
                    container_name = %container.container_name,
                    deployment_id = container.deployment_id.as_deref().unwrap_or("-"),
                    service_name = container.service_name.as_deref().unwrap_or("-"),
                    mesh_address = %address,
                    %error,
                    "could not start a container that failed to start before this node's \
                     WireGuard mesh address existed; start it with `docker start` once the \
                     cause is fixed"
                );
                report.failed.push((container.container_name, error));
            }
        }
    }
    Ok(report)
}

/// Connect to the local Docker daemon and run [`start_stranded_containers`],
/// logging the outcome. Spawned by the network sync loop off its reconcile
/// path once the mesh interface is up.
pub(crate) async fn start_containers_stranded_on_mesh(address: Ipv4Addr) {
    let docker = match Docker::connect_with_local_defaults() {
        Ok(docker) => docker,
        Err(error) => {
            warn!(
                mesh_address = %address,
                %error,
                "could not connect to Docker to start containers that failed to publish on the \
                 WireGuard mesh address before it existed; check `docker ps -a` for containers \
                 exited with \"cannot assign requested address\""
            );
            return;
        }
    };
    match start_stranded_containers(&docker, address).await {
        Ok(report) if report.started.is_empty() && report.failed.is_empty() => {}
        Ok(report) => info!(
            mesh_address = %address,
            started = report.started.len(),
            failed = report.failed.len(),
            "restarted containers whose published ports could not bind the WireGuard mesh \
             address before it existed"
        ),
        Err(error) => warn!(
            mesh_address = %address,
            %error,
            "could not look for containers that failed to publish on the WireGuard mesh address \
             before it existed; check `docker ps -a` for containers exited with \"cannot assign \
             requested address\""
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    const MESH: Ipv4Addr = Ipv4Addr::new(10, 201, 0, 8);

    /// `docker inspect` of a container whose start failed at boot, shaped
    /// like a reported worker reboot: `Exited (128)` with the bind error.
    fn inspect_json(
        id: &str,
        status: &str,
        restart_policy: &str,
        host_ip: &str,
        error: &str,
    ) -> ContainerInspectResponse {
        let json = serde_json::json!({
            "Id": id,
            "Name": format!("/{id}-name"),
            "State": {
                "Status": status,
                "Running": status == "running",
                "ExitCode": 128,
                "Error": error,
            },
            "HostConfig": {
                "RestartPolicy": { "Name": restart_policy, "MaximumRetryCount": 0 },
                "PortBindings": {
                    "80/tcp": [{ "HostIp": host_ip, "HostPort": "6263" }]
                }
            },
            "Config": {
                "Labels": {
                    "sh.temps.managed": "true",
                    "sh.temps.deploy_id": "41",
                }
            }
        });
        serde_json::from_value(json).expect("valid inspect fixture")
    }

    const BIND_ERROR: &str = "failed to set up container networking: driver failed programming \
        external connectivity on endpoint app-1: failed to bind host port 10.201.0.8:6263/tcp: \
        cannot assign requested address";

    #[test]
    fn a_container_that_could_not_bind_the_missing_mesh_address_is_stranded() {
        let inspect = inspect_json("app", "exited", "unless-stopped", "10.201.0.8", BIND_ERROR);

        let stranded = stranded_on(&inspect, MESH).expect("stranded");

        assert_eq!(stranded.container_id, "app");
        assert_eq!(stranded.container_name, "app-name");
        assert_eq!(stranded.deployment_id.as_deref(), Some("41"));
        assert_eq!(stranded.start_error, BIND_ERROR);
    }

    #[test]
    fn every_keep_running_policy_counts_and_created_containers_too() {
        for policy in ["always", "unless-stopped", "on-failure"] {
            for status in ["exited", "created"] {
                let inspect = inspect_json("app", status, policy, "10.201.0.8", BIND_ERROR);
                assert!(
                    stranded_on(&inspect, MESH).is_some(),
                    "policy {policy}, status {status}"
                );
            }
        }
    }

    #[test]
    fn a_container_without_a_restart_policy_is_left_alone() {
        for policy in ["no", ""] {
            let inspect = inspect_json("app", "exited", policy, "10.201.0.8", BIND_ERROR);
            assert_eq!(stranded_on(&inspect, MESH), None, "policy {policy:?}");
        }
    }

    #[test]
    fn a_container_the_operator_stopped_is_left_alone() {
        // `docker stop` leaves no start error behind.
        let inspect = inspect_json("app", "exited", "unless-stopped", "10.201.0.8", "");
        assert_eq!(stranded_on(&inspect, MESH), None);
    }

    #[test]
    fn a_container_that_failed_for_another_reason_is_left_alone() {
        let inspect = inspect_json(
            "app",
            "exited",
            "always",
            "10.201.0.8",
            "driver failed programming external connectivity: Bind for 10.201.0.8:6263 \
             failed: port is already allocated",
        );
        assert_eq!(stranded_on(&inspect, MESH), None);
    }

    #[test]
    fn a_container_publishing_on_another_address_is_left_alone() {
        let inspect = inspect_json("app", "exited", "always", "192.0.2.10", BIND_ERROR);
        assert_eq!(stranded_on(&inspect, MESH), None);
    }

    #[test]
    fn a_running_or_restarting_container_is_left_alone() {
        for status in ["running", "restarting", "paused", "dead"] {
            let inspect = inspect_json("app", status, "always", "10.201.0.8", BIND_ERROR);
            assert_eq!(stranded_on(&inspect, MESH), None, "status {status}");
        }
    }

    #[test]
    fn a_container_without_published_ports_is_left_alone() {
        let mut inspect = inspect_json("app", "exited", "always", "10.201.0.8", BIND_ERROR);
        if let Some(host_config) = inspect.host_config.as_mut() {
            host_config.port_bindings = None;
        }
        assert_eq!(stranded_on(&inspect, MESH), None);
    }

    #[test]
    fn the_check_runs_once_per_mesh_address_and_after_every_rebuild() {
        let other = Ipv4Addr::new(10, 201, 0, 9);
        // First time the mesh is up in this process.
        assert!(should_check(None, MESH, false));
        // Every later tick on the same interface.
        assert!(!should_check(Some(MESH), MESH, false));
        // The interface was recreated, or moved to another address.
        assert!(should_check(Some(MESH), MESH, true));
        assert!(should_check(Some(MESH), other, false));
    }

    /// Docker as seen after a reboot: what it lists as stopped, what it
    /// inspects, and which starts fail.
    struct FakeEngine {
        containers: Vec<ContainerInspectResponse>,
        uninspectable: Vec<String>,
        failing_start: Vec<String>,
        started: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl ContainerEngine for FakeEngine {
        async fn stopped_container_ids(&self) -> Result<Vec<String>, String> {
            Ok(self
                .containers
                .iter()
                .filter_map(|c| c.id.clone())
                .chain(self.uninspectable.iter().cloned())
                .collect())
        }

        async fn inspect(&self, container_id: &str) -> Result<ContainerInspectResponse, String> {
            self.containers
                .iter()
                .find(|c| c.id.as_deref() == Some(container_id))
                .cloned()
                .ok_or_else(|| format!("no such container: {container_id}"))
        }

        async fn start(&self, container_id: &str) -> Result<(), String> {
            if self.failing_start.iter().any(|id| id == container_id) {
                return Err(format!("starting container {container_id}: {BIND_ERROR}"));
            }
            self.started
                .lock()
                .expect("started lock")
                .push(container_id.to_string());
            Ok(())
        }
    }

    #[tokio::test]
    async fn after_the_mesh_is_up_only_the_stranded_containers_are_started() {
        let engine = FakeEngine {
            containers: vec![
                inspect_json("app", "exited", "unless-stopped", "10.201.0.8", BIND_ERROR),
                inspect_json("postgres", "exited", "always", "10.201.0.8", BIND_ERROR),
                inspect_json(
                    "stopped-by-hand",
                    "exited",
                    "unless-stopped",
                    "10.201.0.8",
                    "",
                ),
                inspect_json("no-policy", "exited", "no", "10.201.0.8", BIND_ERROR),
                inspect_json("elsewhere", "exited", "always", "192.0.2.10", BIND_ERROR),
            ],
            uninspectable: vec!["removed-meanwhile".into()],
            failing_start: vec![],
            started: Mutex::default(),
        };

        let report = start_stranded_containers(&engine, MESH)
            .await
            .expect("report");

        assert_eq!(report.started, vec!["app-name", "postgres-name"]);
        assert!(report.failed.is_empty());
        assert_eq!(
            *engine.started.lock().expect("started lock"),
            vec!["app".to_string(), "postgres".to_string()]
        );
    }

    #[tokio::test]
    async fn a_container_that_still_fails_to_start_is_reported_and_the_rest_continue() {
        let engine = FakeEngine {
            containers: vec![
                inspect_json("first", "exited", "always", "10.201.0.8", BIND_ERROR),
                inspect_json("second", "exited", "always", "10.201.0.8", BIND_ERROR),
            ],
            uninspectable: vec![],
            failing_start: vec!["first".into()],
            started: Mutex::default(),
        };

        let report = start_stranded_containers(&engine, MESH)
            .await
            .expect("report");

        assert_eq!(report.started, vec!["second-name"]);
        assert_eq!(report.failed.len(), 1);
        assert_eq!(report.failed[0].0, "first-name");
        assert!(report.failed[0].1.contains("first"));
    }
}
