// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The cross-worker overlay for instances the node runtime creates.
//!
//! The legacy worker deployer attaches every new container to the overlay
//! network (`temps0`) before starting it and installs the per-peer routes
//! inside its network namespace afterwards (see
//! `temps_network::overlay_routes`). Without both, a container can reach
//! peers in this worker's own `/24` but traffic for another worker's
//! compute CIDR falls through the primary network and is dropped: an app
//! cannot reach a database member on another worker by its compute IP.
//!
//! [`OverlayDriver`] gives the node runtime the same behaviour by wrapping
//! its [`RuntimeDriver`]:
//!
//! - `create` adds the overlay to the instance's *optional* networks, which
//!   the Docker driver joins before the instance starts and skips when the
//!   network does not exist. On a worker without an overlay this costs one
//!   refused connect and changes nothing.
//! - `create` first ensures the app network exists when the instance boots
//!   on it, and re-applies the cloud-metadata egress block to it, as the
//!   legacy worker handlers do on every create. Docker refuses to start a
//!   container on a network that does not exist, and nothing else creates
//!   the app network on a worker whose first workload arrives through the
//!   node runtime.
//! - `start` installs the peer routes after the instance is running (the
//!   route install enters the container's network namespace by PID, which
//!   only a running container has). Best-effort, like the legacy deployer:
//!   a failure is logged, and the network-sync loop's sweep repairs routes
//!   of every container on the overlay on its next tick.
//!
//! Everything else is delegated unchanged.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use bollard::models::ContainerInspectResponse;
use temps_workload::driver::{
    Attachments, DriverCapabilities, DriverError, ExecSpec, ExecStream, InstanceHandle,
    InstanceSpec, InstanceStats, InstanceStatus, ListedInstance, LogQuery, LogStream,
    PreparedArtifact, RuntimeDriver, SnapshotRef,
};
use temps_workload::Artifact;

use crate::network_sync::SharedPeers;

/// A [`RuntimeDriver`] that puts new instances on the worker's overlay.
pub struct OverlayDriver {
    inner: Arc<dyn RuntimeDriver>,
    docker: bollard::Docker,
    overlay_network: String,
    peers: SharedPeers,
    /// The app network (`temps-app-network`) created on demand; `None`
    /// leaves every network to the caller.
    app_network: Option<String>,
}

impl OverlayDriver {
    /// `overlay_network` is the Docker network of the overlay (`temps0`);
    /// `peers` is the slot the network-sync loop keeps current.
    pub fn new(
        inner: Arc<dyn RuntimeDriver>,
        docker: bollard::Docker,
        overlay_network: impl Into<String>,
        peers: SharedPeers,
    ) -> Self {
        Self {
            inner,
            docker,
            overlay_network: overlay_network.into(),
            peers,
            app_network: None,
        }
    }

    /// Creates `network` before the first instance that boots on it.
    pub fn with_app_network(mut self, network: impl Into<String>) -> Self {
        self.app_network = Some(network.into());
        self
    }

    /// Ensures the app network exists when `attach` boots on it. A failure
    /// fails the create with the network named, instead of Docker's bare
    /// "network not found" from the start that follows.
    async fn ensure_app_network(
        &self,
        spec: &InstanceSpec,
        attach: &Attachments,
    ) -> Result<(), DriverError> {
        let Some(network) = self.app_network.as_deref() else {
            return Ok(());
        };
        if attach.network.primary.as_deref() != Some(network) {
            return Ok(());
        }
        let outcome = temps_core::docker_network::ensure_bridge_network(&self.docker, network)
            .await
            .map_err(|e| DriverError::Attach {
                driver: self.inner.capabilities().name.clone(),
                instance: spec.name.clone(),
                network: network.to_string(),
                reason: format!("creating the app network failed: {e}"),
            })?;
        if outcome != temps_core::docker_network::NetworkEnsured::Existing {
            tracing::info!(network, instance = %spec.name, ?outcome, "app network created for the node runtime");
        }
        // Re-applied on every create so the block survives a host firewall
        // flush; best-effort, like the legacy handlers.
        if let Err(error) =
            temps_deployer::metadata_egress::apply_metadata_egress_block(&self.docker, network)
                .await
        {
            tracing::warn!(
                network,
                error = %error,
                "Cloud-metadata egress block is incomplete; ensure nftables is installed and \
                 the agent has CAP_NET_ADMIN"
            );
        }
        Ok(())
    }

    /// Installs the peer routes in a running instance. Never fails the
    /// start: see the module documentation.
    async fn install_peer_routes(&self, handle: &InstanceHandle) {
        let peers = self
            .peers
            .read()
            .map(|guard| guard.clone())
            .unwrap_or_else(|poisoned| poisoned.into_inner().clone());
        if peers.is_empty() {
            // No other worker known (single-node, or before the first
            // peer poll): nothing to route.
            return;
        }
        let inspect = match self
            .docker
            .inspect_container(
                &handle.id,
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await
        {
            Ok(inspect) => inspect,
            Err(error) => {
                tracing::warn!(
                    instance = %handle.name,
                    container = %handle.id,
                    overlay = %self.overlay_network,
                    %error,
                    "cannot inspect the instance to install overlay peer routes; the network \
                     sync sweep retries on its next tick"
                );
                return;
            }
        };
        let Some(gateway) = overlay_gateway(&inspect, &self.overlay_network) else {
            // Not on the overlay: this worker has none (yet). Nothing to do.
            tracing::debug!(
                instance = %handle.name,
                overlay = %self.overlay_network,
                "instance is not on the overlay; no peer routes installed"
            );
            return;
        };
        let Some(pid) = running_pid(&inspect) else {
            tracing::debug!(
                instance = %handle.name,
                "instance is not running; peer routes are left to the network sync sweep"
            );
            return;
        };
        // The overlay interface is discovered by the gateway's subnet; the
        // name is only a hint.
        if let Err(error) = temps_network::overlay_routes::install_peer_routes_in_container(
            pid, "eth1", &gateway, &peers,
        )
        .await
        {
            tracing::warn!(
                instance = %handle.name,
                container = %handle.id,
                overlay = %self.overlay_network,
                %gateway,
                peers = peers.len(),
                %error,
                "failed to install overlay peer routes; cross-worker traffic falls through \
                 the primary network until the network sync sweep repairs them"
            );
        }
    }
}

/// `attach` with the overlay added to its optional networks, unless the
/// instance already names it (as its primary, a required or an optional
/// network) or no overlay network is configured.
pub fn with_overlay(attach: &Attachments, overlay_network: &str) -> Attachments {
    let mut attach = attach.clone();
    let network = &mut attach.network;
    let overlay = overlay_network.trim();
    let named = network.primary.as_deref() == Some(overlay)
        || network.required.iter().any(|n| n == overlay)
        || network.optional.iter().any(|n| n == overlay);
    if !overlay.is_empty() && !named {
        network.optional.push(overlay.to_string());
    }
    attach
}

/// The container's gateway on the overlay network, when it is attached.
fn overlay_gateway(inspect: &ContainerInspectResponse, overlay_network: &str) -> Option<String> {
    inspect
        .network_settings
        .as_ref()?
        .networks
        .as_ref()?
        .get(overlay_network)?
        .gateway
        .as_deref()
        .map(str::trim)
        .filter(|g| !g.is_empty())
        .map(str::to_string)
}

/// The container's init PID while it runs.
fn running_pid(inspect: &ContainerInspectResponse) -> Option<i32> {
    inspect
        .state
        .as_ref()?
        .pid
        .filter(|p| *p > 0)
        .and_then(|p| i32::try_from(p).ok())
}

#[async_trait]
impl RuntimeDriver for OverlayDriver {
    fn capabilities(&self) -> &DriverCapabilities {
        self.inner.capabilities()
    }

    async fn prepare(&self, artifact: &Artifact) -> Result<PreparedArtifact, DriverError> {
        self.inner.prepare(artifact).await
    }

    async fn create(
        &self,
        spec: &InstanceSpec,
        attach: &Attachments,
    ) -> Result<InstanceHandle, DriverError> {
        self.ensure_app_network(spec, attach).await?;
        let attach = with_overlay(attach, &self.overlay_network);
        self.inner.create(spec, &attach).await
    }

    async fn start(&self, handle: &InstanceHandle) -> Result<(), DriverError> {
        self.inner.start(handle).await?;
        self.install_peer_routes(handle).await;
        Ok(())
    }

    async fn stop(&self, handle: &InstanceHandle) -> Result<(), DriverError> {
        self.inner.stop(handle).await
    }

    async fn remove(&self, handle: &InstanceHandle, purge: bool) -> Result<(), DriverError> {
        self.inner.remove(handle, purge).await
    }

    async fn inspect(&self, handle: &InstanceHandle) -> Result<InstanceStatus, DriverError> {
        self.inner.inspect(handle).await
    }

    async fn logs(
        &self,
        handle: &InstanceHandle,
        query: LogQuery,
    ) -> Result<LogStream, DriverError> {
        self.inner.logs(handle, query).await
    }

    async fn stats(&self, handle: &InstanceHandle) -> Result<InstanceStats, DriverError> {
        self.inner.stats(handle).await
    }

    async fn exec(
        &self,
        handle: &InstanceHandle,
        cmd: ExecSpec,
    ) -> Result<ExecStream, DriverError> {
        self.inner.exec(handle, cmd).await
    }

    async fn snapshot(&self, handle: &InstanceHandle) -> Result<SnapshotRef, DriverError> {
        self.inner.snapshot(handle).await
    }

    async fn list(
        &self,
        labels: &BTreeMap<String, String>,
    ) -> Result<Vec<ListedInstance>, DriverError> {
        self.inner.list(labels).await
    }

    async fn wait(&self, handle: &InstanceHandle) -> Result<(), DriverError> {
        self.inner.wait(handle).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bollard::models::{ContainerState, EndpointSettings, NetworkSettings};
    use std::collections::HashMap;
    use std::sync::Mutex;
    use temps_workload::driver::NetworkAttachment;

    const OVERLAY: &str = "temps0";

    fn attach(primary: &str) -> Attachments {
        Attachments {
            network: NetworkAttachment {
                primary: Some(primary.to_string()),
                required: vec!["shop-db".to_string()],
                optional: Vec::new(),
                dns: Some(vec!["10.237.1.1".to_string()]),
            },
            volumes: vec!["data:/data".to_string()],
        }
    }

    #[test]
    fn new_instances_join_the_overlay_as_an_optional_network() {
        let attached = with_overlay(&attach("temps-app-network"), OVERLAY);
        assert_eq!(attached.network.optional, [OVERLAY]);
        // Everything else is the caller's, untouched.
        assert_eq!(
            attached.network.primary.as_deref(),
            Some("temps-app-network")
        );
        assert_eq!(attached.network.required, ["shop-db"]);
        assert_eq!(
            attached.network.dns.as_deref(),
            Some(["10.237.1.1".to_string()].as_slice())
        );
        assert_eq!(attached.volumes, ["data:/data"]);
    }

    #[test]
    fn an_instance_that_already_names_the_overlay_is_not_attached_twice() {
        let on_overlay = with_overlay(&attach(OVERLAY), OVERLAY);
        assert!(on_overlay.network.optional.is_empty());

        let mut required = attach("temps-app-network");
        required.network.required.push(OVERLAY.to_string());
        assert!(with_overlay(&required, OVERLAY).network.optional.is_empty());

        let mut optional = attach("temps-app-network");
        optional.network.optional.push(OVERLAY.to_string());
        assert_eq!(with_overlay(&optional, OVERLAY).network.optional, [OVERLAY]);
    }

    #[test]
    fn no_overlay_name_adds_nothing() {
        let attached = with_overlay(&attach("temps-app-network"), "  ");
        assert_eq!(attached, attach("temps-app-network"));
    }

    fn inspect(networks: &[(&str, Option<&str>)], pid: Option<i64>) -> ContainerInspectResponse {
        let networks: HashMap<String, EndpointSettings> = networks
            .iter()
            .map(|(name, gateway)| {
                (
                    name.to_string(),
                    EndpointSettings {
                        gateway: gateway.map(str::to_string),
                        ..Default::default()
                    },
                )
            })
            .collect();
        ContainerInspectResponse {
            state: Some(ContainerState {
                pid,
                ..Default::default()
            }),
            network_settings: Some(NetworkSettings {
                networks: Some(networks),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn the_gateway_is_read_from_the_overlay_endpoint_only() {
        let both = inspect(
            &[
                ("temps-app-network", Some("172.18.0.1")),
                (OVERLAY, Some("10.237.4.1")),
            ],
            Some(4242),
        );
        assert_eq!(
            overlay_gateway(&both, OVERLAY).as_deref(),
            Some("10.237.4.1")
        );
        assert_eq!(running_pid(&both), Some(4242));

        let primary_only = inspect(&[("temps-app-network", Some("172.18.0.1"))], Some(1));
        assert_eq!(overlay_gateway(&primary_only, OVERLAY), None);

        let no_gateway = inspect(&[(OVERLAY, Some(""))], Some(1));
        assert_eq!(overlay_gateway(&no_gateway, OVERLAY), None);
        assert_eq!(
            overlay_gateway(&ContainerInspectResponse::default(), OVERLAY),
            None
        );
    }

    #[test]
    fn a_stopped_container_has_no_pid_to_enter() {
        assert_eq!(running_pid(&inspect(&[], Some(0))), None);
        assert_eq!(running_pid(&inspect(&[], None)), None);
        assert_eq!(running_pid(&ContainerInspectResponse::default()), None);
    }

    /// Records what the node runtime would have asked Docker for.
    struct RecordingDriver {
        capabilities: DriverCapabilities,
        created: Mutex<Vec<Attachments>>,
        started: Mutex<Vec<String>>,
    }

    fn unused(operation: &'static str) -> DriverError {
        DriverError::Unsupported {
            driver: "recording".to_string(),
            operation,
            instance: String::new(),
        }
    }

    #[async_trait]
    impl RuntimeDriver for RecordingDriver {
        fn capabilities(&self) -> &DriverCapabilities {
            &self.capabilities
        }
        async fn prepare(&self, _: &Artifact) -> Result<PreparedArtifact, DriverError> {
            Err(unused("prepare"))
        }
        async fn create(
            &self,
            spec: &InstanceSpec,
            attach: &Attachments,
        ) -> Result<InstanceHandle, DriverError> {
            if let Ok(mut created) = self.created.lock() {
                created.push(attach.clone());
            }
            Ok(InstanceHandle {
                driver: "recording".to_string(),
                id: format!("id-{}", spec.name),
                name: spec.name.clone(),
            })
        }
        async fn start(&self, handle: &InstanceHandle) -> Result<(), DriverError> {
            if let Ok(mut started) = self.started.lock() {
                started.push(handle.name.clone());
            }
            Ok(())
        }
        async fn stop(&self, _: &InstanceHandle) -> Result<(), DriverError> {
            Err(unused("stop"))
        }
        async fn remove(&self, _: &InstanceHandle, _: bool) -> Result<(), DriverError> {
            Err(unused("remove"))
        }
        async fn inspect(&self, _: &InstanceHandle) -> Result<InstanceStatus, DriverError> {
            Err(unused("inspect"))
        }
        async fn logs(&self, _: &InstanceHandle, _: LogQuery) -> Result<LogStream, DriverError> {
            Err(unused("logs"))
        }
        async fn stats(&self, _: &InstanceHandle) -> Result<InstanceStats, DriverError> {
            Err(unused("stats"))
        }
    }

    fn spec(name: &str) -> InstanceSpec {
        InstanceSpec {
            owner: temps_workload::WorkloadRef::new(temps_workload::WorkloadKind::Deployment, 7),
            name: name.to_string(),
            image: "app:1".to_string(),
            platform: None,
            env: Vec::new(),
            secret_files: Vec::new(),
            command: None,
            entrypoint: None,
            workdir: None,
            user: None,
            ports: Vec::new(),
            restart: Default::default(),
            limits: Default::default(),
            log_rotation: None,
            labels: BTreeMap::new(),
            security: temps_workload::SecurityProfile::Trusted,
        }
    }

    /// A client that is never dialled: with no peers, start never inspects.
    fn idle_docker() -> bollard::Docker {
        bollard::Docker::connect_with_http("http://127.0.0.1:9", 1, bollard::API_DEFAULT_VERSION)
            .unwrap_or_else(|e| panic!("building an idle Docker client: {e}"))
    }

    #[tokio::test]
    async fn the_wrapped_driver_creates_on_the_overlay_and_starts_without_peers() {
        let inner = Arc::new(RecordingDriver {
            capabilities: temps_workload_docker::capabilities(),
            created: Mutex::new(Vec::new()),
            started: Mutex::new(Vec::new()),
        });
        let driver = OverlayDriver::new(
            inner.clone(),
            idle_docker(),
            OVERLAY,
            Arc::new(std::sync::RwLock::new(Vec::new())),
        );
        let handle = driver
            .create(&spec("app-7-1"), &attach("temps-app-network"))
            .await
            .unwrap_or_else(|e| panic!("create: {e}"));
        driver
            .start(&handle)
            .await
            .unwrap_or_else(|e| panic!("start: {e}"));

        let created = inner.created.lock().map(|c| c.clone()).unwrap_or_default();
        assert_eq!(created.len(), 1);
        assert_eq!(created[0].network.optional, [OVERLAY]);
        assert_eq!(
            inner.started.lock().map(|s| s.clone()).unwrap_or_default(),
            ["app-7-1"]
        );
    }

    fn recording() -> Arc<RecordingDriver> {
        Arc::new(RecordingDriver {
            capabilities: temps_workload_docker::capabilities(),
            created: Mutex::new(Vec::new()),
            started: Mutex::new(Vec::new()),
        })
    }

    #[tokio::test]
    async fn an_app_network_that_cannot_be_ensured_fails_the_create_naming_it() {
        let inner = recording();
        let driver = OverlayDriver::new(
            inner.clone(),
            idle_docker(),
            OVERLAY,
            Arc::new(std::sync::RwLock::new(Vec::new())),
        )
        .with_app_network("temps-app-network");

        let error = driver
            .create(&spec("app-7-1"), &attach("temps-app-network"))
            .await
            .err()
            .unwrap_or_else(|| panic!("create must fail when the daemon is unreachable"));

        match error {
            DriverError::Attach {
                network, instance, ..
            } => {
                assert_eq!(network, "temps-app-network");
                assert_eq!(instance, "app-7-1");
            }
            other => panic!("expected an Attach error, got {other}"),
        }
        assert!(inner.created.lock().map(|c| c.is_empty()).unwrap_or(false));
    }

    #[tokio::test]
    async fn instances_on_another_network_never_dial_the_daemon_for_the_app_network() {
        let inner = recording();
        let driver = OverlayDriver::new(
            inner.clone(),
            idle_docker(),
            OVERLAY,
            Arc::new(std::sync::RwLock::new(Vec::new())),
        )
        .with_app_network("temps-app-network");

        driver
            .create(&spec("job-3-1"), &attach("some-other-network"))
            .await
            .unwrap_or_else(|e| panic!("create: {e}"));

        assert_eq!(inner.created.lock().map(|c| c.len()).unwrap_or(0), 1);
    }

    /// A worker whose first workload arrives through the node runtime has
    /// no app network yet; the create must make it rather than leave the
    /// start to fail with "network not found". Skips without Docker.
    #[tokio::test]
    async fn a_missing_app_network_is_created_before_the_first_instance() {
        let Ok(docker) = bollard::Docker::connect_with_defaults() else {
            println!("Docker not available, skipping");
            return;
        };
        if docker.ping().await.is_err() {
            println!("Docker not available, skipping");
            return;
        }
        let network = format!("temps-test-app-net-{}", std::process::id());
        let _ = docker.remove_network(&network).await;

        let inner = recording();
        let driver = OverlayDriver::new(
            inner.clone(),
            docker.clone(),
            OVERLAY,
            Arc::new(std::sync::RwLock::new(Vec::new())),
        )
        .with_app_network(network.clone());
        let created = driver.create(&spec("app-7-1"), &attach(&network)).await;
        let exists = docker
            .inspect_network(
                &network,
                None::<bollard::query_parameters::InspectNetworkOptions>,
            )
            .await
            .map(|_| true);
        let _ = docker.remove_network(&network).await;

        // A daemon with no free address pool cannot create any network;
        // that is the host, not the driver.
        if let Err(e) = &created {
            if e.to_string()
                .contains("address pools have been fully subnetted")
            {
                println!("Docker has no free address pool, skipping: {e}");
                return;
            }
        }
        created.unwrap_or_else(|e| panic!("create: {e}"));
        assert!(
            matches!(exists, Ok(true)),
            "network {network} should exist after the create, got {exists:?}"
        );
        assert_eq!(inner.created.lock().map(|c| c.len()).unwrap_or(0), 1);
    }
}
