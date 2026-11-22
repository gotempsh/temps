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
//!
//! A failed Docker listing is retried with backoff and the address is only
//! recorded as checked once a listing succeeds, so a Docker daemon that is
//! slow to answer at boot does not leave the containers stopped. Once it has
//! started a container the scan installs the overlay peer routes inside
//! every overlay container, with the inputs the sync loop's last route sweep
//! used: that sweep may have listed the running containers before the start,
//! and an agent running from its offline snapshot sweeps only once.

use std::collections::{HashMap, HashSet};
use std::net::Ipv4Addr;
use std::time::Duration;

use async_trait::async_trait;
use bollard::models::{ContainerInspectResponse, ContainerStateStatusEnum, RestartPolicyNameEnum};
use bollard::Docker;
use temps_core::retry::RetryConfig;
use temps_network::Peer;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tracing::{info, warn};

/// How long a scan waits for the sync loop's first route sweep before
/// leaving the routes of the containers it started to a later sweep. The
/// sweep runs in the same sync tick that launches the scan, so this is only
/// reached when that tick failed to apply the overlay.
const ROUTE_SWEEP_WAIT: Duration = Duration::from_secs(300);

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
    /// Install the overlay peer routes inside every running overlay
    /// container (`ip route replace`, so repeating it changes nothing).
    async fn sweep_routes(&self, routes: &RouteSweep) -> Result<(), String>;
}

/// The inputs of the sync loop's container route sweep, published by every
/// sweep so a scan can repeat it for the containers it started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RouteSweep {
    /// The overlay's Docker network.
    pub overlay_network: String,
    /// This node's overlay bridge address, the containers' gateway.
    pub bridge_gateway: String,
    /// The peers whose subnets are routed through the gateway.
    pub peers: Vec<Peer>,
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

    async fn sweep_routes(&self, routes: &RouteSweep) -> Result<(), String> {
        temps_network::overlay_routes::sweep_container_routes(
            self,
            &routes.overlay_network,
            &routes.bridge_gateway,
            &routes.peers,
            // A full pass, like the sync loop's.
            &mut HashSet::new(),
        )
        .await
        .map_err(|e| {
            format!(
                "sweeping container routes on {}: {e}",
                routes.overlay_network
            )
        })
    }
}

/// When the sync loop scans for stranded containers: once per mesh address
/// this process brought up, and again after every interface rebuild (anything
/// Docker tried to start while the interface was gone failed the same way).
/// An address counts as scanned only once a scan's Docker listing succeeded,
/// and a second scan never runs alongside the first.
#[derive(Debug, Default)]
pub(crate) struct StrandedScan {
    /// The mesh address the last successful scan covered.
    completed_for: Option<Ipv4Addr>,
    /// An interface rebuild that no scan launched since has covered.
    rebuild_pending: bool,
    /// The running scan and the address it covers. Its output is whether
    /// its Docker listing succeeded.
    running: Option<(Ipv4Addr, JoinHandle<bool>)>,
}

impl StrandedScan {
    /// Called on every tick the mesh interface is up on `address`; launches
    /// a scan through `spawn` when one is due.
    pub(crate) async fn on_mesh_ready<F>(&mut self, address: Ipv4Addr, rebuilt: bool, spawn: F)
    where
        F: FnOnce(Ipv4Addr) -> JoinHandle<bool>,
    {
        self.rebuild_pending |= rebuilt;
        if let Some((scanned, handle)) = self.running.take() {
            if !handle.is_finished() {
                self.running = Some((scanned, handle));
                return;
            }
            // Finished, so this does not wait. A panicked scan counts as
            // failed and runs again.
            if handle.await.unwrap_or(false) {
                self.completed_for = Some(scanned);
            }
        }
        if self.rebuild_pending || self.completed_for != Some(address) {
            self.rebuild_pending = false;
            self.running = Some((address, spawn(address)));
        }
    }
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

/// Retries of a failed Docker listing: about two minutes in all. After the
/// last one the scan reports failure and the sync loop runs it again on its
/// next tick.
fn listing_retry() -> RetryConfig {
    RetryConfig::new(6)
        .with_base_delay(Duration::from_secs(5))
        .with_max_delay(Duration::from_secs(60))
}

/// One scan: start the stranded containers, retrying a failed listing per
/// `retry`, then install the overlay routes of the containers it started.
/// Returns whether the listing succeeded, i.e. whether the address is
/// covered.
pub(crate) async fn run_scan(
    engine: &dyn ContainerEngine,
    address: Ipv4Addr,
    retry: &RetryConfig,
    routes: watch::Receiver<Option<RouteSweep>>,
    route_wait: Duration,
) -> bool {
    let report = match retry
        .retry(|| start_stranded_containers(engine, address))
        .await
    {
        Ok(report) => report,
        Err(error) => {
            warn!(
                mesh_address = %address,
                %error,
                attempts = retry.max_attempts,
                "could not look for containers that failed to publish on the WireGuard mesh \
                 address before it existed; retrying on the next network sync. Meanwhile check \
                 `docker ps -a` for containers exited with \"cannot assign requested address\""
            );
            return false;
        }
    };
    if report.started.is_empty() && report.failed.is_empty() {
        return true;
    }
    info!(
        mesh_address = %address,
        started = report.started.len(),
        failed = report.failed.len(),
        "restarted containers whose published ports could not bind the WireGuard mesh \
         address before it existed"
    );
    if !report.started.is_empty() {
        install_routes(engine, address, routes, route_wait).await;
    }
    true
}

/// Repeat the sync loop's route sweep for containers a scan started,
/// waiting up to `wait` for the loop to publish its first sweep.
async fn install_routes(
    engine: &dyn ContainerEngine,
    address: Ipv4Addr,
    mut routes: watch::Receiver<Option<RouteSweep>>,
    wait: Duration,
) {
    let published = tokio::time::timeout(wait, routes.wait_for(Option::is_some))
        .await
        .ok()
        .and_then(|waited| waited.ok().map(|sweep| Option::clone(&sweep)))
        .flatten();
    let Some(sweep) = published else {
        warn!(
            mesh_address = %address,
            wait_secs = wait.as_secs(),
            "the overlay was not applied while starting containers stranded by the missing \
             WireGuard mesh address; their peer routes are installed by the next network sync \
             that applies it"
        );
        return;
    };
    if let Err(error) = engine.sweep_routes(&sweep).await {
        warn!(
            mesh_address = %address,
            %error,
            "could not install overlay peer routes in containers started after the WireGuard \
             mesh address came up; the next network sync retries"
        );
    }
}

/// Connect to the local Docker daemon and run [`run_scan`]. Spawned by the
/// network sync loop off its reconcile path once the mesh interface is up;
/// its output is whether the address is covered (see [`StrandedScan`]).
pub(crate) async fn start_containers_stranded_on_mesh(
    address: Ipv4Addr,
    routes: watch::Receiver<Option<RouteSweep>>,
) -> bool {
    let docker = match Docker::connect_with_local_defaults() {
        Ok(docker) => docker,
        Err(error) => {
            warn!(
                mesh_address = %address,
                %error,
                "could not connect to Docker to start containers that failed to publish on the \
                 WireGuard mesh address before it existed; retrying on the next network sync. \
                 Meanwhile check `docker ps -a` for containers exited with \"cannot assign \
                 requested address\""
            );
            return false;
        }
    };
    run_scan(&docker, address, &listing_retry(), routes, ROUTE_SWEEP_WAIT).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
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

    /// A scan that has already finished with `outcome`.
    fn finished(outcome: bool) -> impl FnOnce(Ipv4Addr) -> JoinHandle<bool> {
        move |_| tokio::spawn(async move { outcome })
    }

    /// Wait for the running scan, if any, to finish.
    async fn settle(scan: &StrandedScan) {
        while scan.running.as_ref().is_some_and(|(_, h)| !h.is_finished()) {
            tokio::task::yield_now().await;
        }
    }

    /// Run `on_mesh_ready` and report whether it launched a scan.
    async fn ready(
        scan: &mut StrandedScan,
        address: Ipv4Addr,
        rebuilt: bool,
        outcome: bool,
    ) -> bool {
        let launched = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let seen = launched.clone();
        let spawn = finished(outcome);
        scan.on_mesh_ready(address, rebuilt, move |address| {
            seen.store(true, Ordering::SeqCst);
            spawn(address)
        })
        .await;
        settle(scan).await;
        launched.load(Ordering::SeqCst)
    }

    #[tokio::test]
    async fn the_scan_runs_once_per_mesh_address_and_after_every_rebuild() {
        let other = Ipv4Addr::new(10, 201, 0, 9);
        let mut scan = StrandedScan::default();
        // First time the mesh is up in this process.
        assert!(ready(&mut scan, MESH, false, true).await);
        // Every later tick on the same interface.
        assert!(!ready(&mut scan, MESH, false, true).await);
        assert!(!ready(&mut scan, MESH, false, true).await);
        // The interface was recreated, or moved to another address.
        assert!(ready(&mut scan, MESH, true, true).await);
        assert!(!ready(&mut scan, MESH, false, true).await);
        assert!(ready(&mut scan, other, false, true).await);
        assert!(!ready(&mut scan, other, false, true).await);
    }

    #[tokio::test]
    async fn a_scan_whose_docker_listing_failed_runs_again_on_the_next_tick() {
        let mut scan = StrandedScan::default();
        assert!(ready(&mut scan, MESH, false, false).await);
        // Docker did not answer: the address is not covered yet.
        assert!(ready(&mut scan, MESH, false, false).await);
        assert!(ready(&mut scan, MESH, false, true).await);
        // Covered now; an empty successful scan is not repeated.
        assert!(!ready(&mut scan, MESH, false, true).await);
    }

    #[tokio::test]
    async fn no_second_scan_runs_alongside_one_and_a_rebuild_meanwhile_is_kept() {
        let mut scan = StrandedScan::default();
        let (release, released) = tokio::sync::oneshot::channel::<()>();
        scan.on_mesh_ready(MESH, false, move |_| {
            tokio::spawn(async move { released.await.is_ok() })
        })
        .await;
        assert!(scan.running.is_some());

        // Still listing: neither another tick nor a rebuild starts a second
        // scan.
        let mut launched = false;
        scan.on_mesh_ready(MESH, true, |address| {
            launched = true;
            finished(true)(address)
        })
        .await;
        assert!(!launched);

        release.send(()).expect("scan still waiting");
        settle(&scan).await;
        // The first scan covered the address, but the rebuild seen while it
        // ran happened after its listing could have started: scan again.
        assert!(ready(&mut scan, MESH, false, true).await);
        assert!(!ready(&mut scan, MESH, false, true).await);
    }

    #[tokio::test]
    async fn a_panicked_scan_counts_as_failed() {
        let mut scan = StrandedScan::default();
        scan.on_mesh_ready(MESH, false, |_| {
            tokio::spawn(async { panic!("scan task panicked") })
        })
        .await;
        settle(&scan).await;
        assert!(ready(&mut scan, MESH, false, true).await);
    }

    /// Docker as seen after a reboot: what it lists as stopped, what it
    /// inspects, and which starts fail.
    #[derive(Default)]
    struct FakeEngine {
        containers: Vec<ContainerInspectResponse>,
        uninspectable: Vec<String>,
        failing_start: Vec<String>,
        /// How many listings fail before one succeeds.
        failing_listings: AtomicU32,
        started: Mutex<Vec<String>>,
        swept: Mutex<Vec<RouteSweep>>,
    }

    #[async_trait]
    impl ContainerEngine for FakeEngine {
        async fn stopped_container_ids(&self) -> Result<Vec<String>, String> {
            let failing = self.failing_listings.load(Ordering::SeqCst);
            if failing > 0 {
                self.failing_listings.store(failing - 1, Ordering::SeqCst);
                return Err("listing stopped containers: connection refused".into());
            }
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

        async fn sweep_routes(&self, routes: &RouteSweep) -> Result<(), String> {
            self.swept.lock().expect("swept lock").push(routes.clone());
            Ok(())
        }
    }

    fn sweep() -> RouteSweep {
        RouteSweep {
            overlay_network: "temps-overlay".into(),
            bridge_gateway: "10.100.3.1".into(),
            peers: vec![],
        }
    }

    fn quick_retry(attempts: u32) -> RetryConfig {
        RetryConfig::new(attempts).with_base_delay(Duration::from_secs(5))
    }

    fn stranded_app() -> FakeEngine {
        FakeEngine {
            containers: vec![inspect_json(
                "app",
                "exited",
                "unless-stopped",
                "10.201.0.8",
                BIND_ERROR,
            )],
            ..FakeEngine::default()
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_docker_listing_that_fails_at_first_is_retried() {
        let engine = stranded_app();
        engine.failing_listings.store(2, Ordering::SeqCst);
        let (_tx, routes) = watch::channel(Some(sweep()));

        assert!(
            run_scan(
                &engine,
                MESH,
                &quick_retry(3),
                routes,
                Duration::from_secs(1)
            )
            .await
        );
        assert_eq!(*engine.started.lock().expect("started lock"), vec!["app"]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_docker_listing_that_keeps_failing_reports_the_address_uncovered() {
        let engine = stranded_app();
        engine.failing_listings.store(3, Ordering::SeqCst);
        let (_tx, routes) = watch::channel(Some(sweep()));

        assert!(
            !run_scan(
                &engine,
                MESH,
                &quick_retry(3),
                routes,
                Duration::from_secs(1)
            )
            .await
        );
        assert!(engine.started.lock().expect("started lock").is_empty());
    }

    #[tokio::test]
    async fn started_containers_get_their_overlay_routes_from_the_last_sweep() {
        let engine = stranded_app();
        // The sync loop swept before the scan's start finished.
        let (_tx, routes) = watch::channel(Some(sweep()));

        assert!(
            run_scan(
                &engine,
                MESH,
                &quick_retry(1),
                routes,
                Duration::from_secs(1)
            )
            .await
        );
        assert_eq!(*engine.swept.lock().expect("swept lock"), vec![sweep()]);
    }

    #[tokio::test(start_paused = true)]
    async fn started_containers_get_their_overlay_routes_once_the_loop_first_sweeps() {
        let engine = std::sync::Arc::new(stranded_app());
        // The scan finishes before the sync loop's first sweep.
        let (tx, routes) = watch::channel(None);
        let scan = tokio::spawn({
            let engine = engine.clone();
            async move {
                run_scan(
                    engine.as_ref(),
                    MESH,
                    &quick_retry(1),
                    routes,
                    Duration::from_secs(60),
                )
                .await
            }
        });
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert!(engine.swept.lock().expect("swept lock").is_empty());

        tx.send_replace(Some(sweep()));
        assert!(scan.await.expect("scan task"));
        assert_eq!(*engine.swept.lock().expect("swept lock"), vec![sweep()]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_scan_that_started_nothing_does_not_sweep_or_wait() {
        let engine = FakeEngine {
            containers: vec![inspect_json(
                "stopped-by-hand",
                "exited",
                "unless-stopped",
                "10.201.0.8",
                "",
            )],
            ..FakeEngine::default()
        };
        let (_tx, routes) = watch::channel(None);

        assert!(
            run_scan(
                &engine,
                MESH,
                &quick_retry(1),
                routes,
                Duration::from_secs(60)
            )
            .await
        );
        assert!(engine.swept.lock().expect("swept lock").is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn a_scan_gives_up_waiting_for_a_sweep_but_still_covers_the_address() {
        let engine = stranded_app();
        let (_tx, routes) = watch::channel(None);

        assert!(
            run_scan(
                &engine,
                MESH,
                &quick_retry(1),
                routes,
                Duration::from_secs(60)
            )
            .await
        );
        assert_eq!(*engine.started.lock().expect("started lock"), vec!["app"]);
        assert!(engine.swept.lock().expect("swept lock").is_empty());
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
            ..FakeEngine::default()
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
            ..FakeEngine::default()
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
