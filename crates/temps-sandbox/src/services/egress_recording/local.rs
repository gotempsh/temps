// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The control plane's egress recorder: the per-node egress proxy for the
//! sandboxes that run on this host and record their outbound connections.
//!
//! **Only opted-in sandboxes go through it.** Every other sandbox keeps
//! egressing through its own sidecar exactly as before. A recorded
//! sandbox's sidecar keeps the Temps relays and internal names
//! (`*.temps.local`), and hands every other outbound request to this proxy
//! (`TEMPS_EGRESS_RECORDING_PROXY_ADDRESS`), which applies the sandbox's own
//! egress policy ([`super::policy`]) and records each connection, allowed
//! or refused, into the ingest service ([`super::sink::LocalRecordSink`]).
//!
//! **Who is calling** comes from the source address: the proxy listens on
//! the host's address on the sidecars' shared egress network, and a
//! sidecar reaches it from its own address there, which only Docker
//! assigns. The table maps that address to the sandbox the sidecar serves
//! (`egress_proxy_sandbox`, the same Docker-recorded identity the internal
//! proxy trusts, ADR 105). An address no recorded sandbox holds is refused.
//!
//! **Linux only**, where the control plane runs the sandboxes' Docker; node
//! 0 reports that it can record only once the proxy serves.
//!
//! **Its port is stable** ([`RECORDING_PROXY_PORT`]): a recorded sandbox's
//! sidecar carries the address, so a port that changed on every restart
//! would replace every recorded sidecar on every restart. Only when the
//! port is taken does the proxy take any free one, saying so.
//!
//! **Cost.** Each refresh is one query for the opted-in sandboxes of this
//! host; only when there are some, one local container list. Every
//! [`REFRESH_INTERVAL`], and at once after an opt-in changes.

use std::collections::HashSet;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use async_trait::async_trait;
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter};
use temps_entities::sandboxes;
use temps_workload::{WorkloadKind, WorkloadRef};
use temps_workload_netpol::{
    recording_channel, EgressBatcher, EgressProxy, EgressProxyConfig, EgressRecordingCapability,
    EgressTable, IdentityMap,
};
use tokio::sync::Notify;
use tracing::{debug, info, warn};

use super::sink::LocalRecordSink;
use super::{policy, EgressRecordingHooks, EgressRecordingService};

/// How often the table follows sidecars and opt-ins without being told.
pub const REFRESH_INTERVAL: Duration = Duration::from_secs(5);

/// The port the recorder listens on, on the host's address on the sandbox
/// egress network (reachable only from that network's sidecars).
pub const RECORDING_PROXY_PORT: u16 = 13128;

/// Container-name prefix of a sandbox (`temps-sandbox-<label>`).
const SANDBOX_CONTAINER_PREFIX: &str = "temps-sandbox-";

/// Re-applies a sandbox's egress so its sidecar is recreated with or
/// without the hand-off to the recorder.
#[async_trait]
pub trait SidecarRefresh: Send + Sync {
    async fn refresh(&self, row: &sandboxes::Model) -> Result<(), String>;
}

#[async_trait]
impl SidecarRefresh for crate::services::registry::StandaloneSandboxRegistry {
    async fn refresh(&self, row: &sandboxes::Model) -> Result<(), String> {
        self.set_egress_policy(row.id, &row.public_id, &row.egress_policy.0)
            .await
            .map_err(|e| e.to_string())
    }
}

/// The node-0 status setter: [`temps_network_policy::NetworkPolicyService`].
pub trait LocalCapability: Send + Sync {
    fn set(&self, capability: EgressRecordingCapability);
}

impl LocalCapability for temps_network_policy::NetworkPolicyService {
    fn set(&self, capability: EgressRecordingCapability) {
        self.set_local_egress_recording(capability);
    }
}

/// The running recorder, shared by its tasks and the opt-in hooks.
pub struct LocalEgressRecorder {
    db: Arc<DatabaseConnection>,
    docker: bollard::Docker,
    table: Arc<ArcSwap<EgressTable>>,
    address: std::sync::Mutex<Option<SocketAddr>>,
    wake: Notify,
    /// Sandboxes whose sidecar could not be refreshed: retried on every
    /// refresh until it works, so a failed replacement never leaves a
    /// sandbox without a way out for long.
    pending: std::sync::Mutex<HashSet<i32>>,
}

/// The sandbox container name of a public id.
pub fn container_name(public_id: &str) -> String {
    format!(
        "{SANDBOX_CONTAINER_PREFIX}{}",
        public_id.strip_prefix("sbx_").unwrap_or(public_id)
    )
}

impl LocalEgressRecorder {
    /// Starts the recorder in the background. It waits for the sidecars'
    /// egress network to exist (it is created with the first sandbox),
    /// then serves, and only then reports that node 0 can record.
    pub fn spawn(
        db: Arc<DatabaseConnection>,
        docker: bollard::Docker,
        service: Arc<EgressRecordingService>,
        capability: Arc<dyn LocalCapability>,
        sidecars: Option<Arc<dyn SidecarRefresh>>,
    ) -> Arc<Self> {
        let recorder = Arc::new(Self {
            db,
            docker,
            table: Arc::new(ArcSwap::from_pointee(EgressTable::default())),
            address: std::sync::Mutex::new(None),
            wake: Notify::new(),
            pending: std::sync::Mutex::new(HashSet::new()),
        });
        if !cfg!(target_os = "linux") {
            info!(
                os = std::env::consts::OS,
                "egress recording: the control plane records only on Linux"
            );
            temps_sandbox_providers::docker::publish_egress_recording(None);
            return recorder;
        }
        let running = recorder.clone();
        tokio::spawn(async move { running.run(service, capability, sidecars).await });
        recorder
    }

    async fn run(
        self: Arc<Self>,
        service: Arc<EgressRecordingService>,
        capability: Arc<dyn LocalCapability>,
        sidecars: Option<Arc<dyn SidecarRefresh>>,
    ) {
        let mut waiting_logged = false;
        loop {
            let gateway = match temps_sandbox_providers::docker::egress_network_gateway(
                &self.docker,
            )
            .await
            {
                Ok(Some(gateway)) => gateway,
                Ok(None) => {
                    if !waiting_logged {
                        info!(
                            network = temps_sandbox_providers::docker::SANDBOX_EGRESS_NETWORK,
                            "egress recording waits for the sandbox egress network, created with \
                             the first sandbox"
                        );
                        waiting_logged = true;
                    }
                    self.sleep().await;
                    continue;
                }
                Err(error) => {
                    warn!(%error, "egress recording: could not inspect the sandbox egress network");
                    self.sleep().await;
                    continue;
                }
            };
            match self.serve(gateway, &service).await {
                Ok((address, serving)) => {
                    *self.address.lock().unwrap_or_else(|e| e.into_inner()) = Some(address);
                    self.refresh().await;
                    capability.set(EgressRecordingCapability::detect(true));
                    info!(%address, "egress recording proxy serving the control plane's sandboxes");
                    if let Some(sidecars) = &sidecars {
                        self.refresh_recorded_sidecars(sidecars.as_ref()).await;
                    }
                    tokio::select! {
                        _ = serving => {}
                        _ = self.refresh_loop(sidecars.as_deref()) => {}
                    }
                    warn!(%address, "egress recording proxy stopped; restarting it");
                    capability.set(EgressRecordingCapability::detect(false));
                    *self.address.lock().unwrap_or_else(|e| e.into_inner()) = None;
                    temps_sandbox_providers::docker::publish_egress_recording(None);
                }
                Err(error) => {
                    warn!(%error, "egress recording proxy could not start");
                    // Sidecars stop handing traffic to a proxy that is not
                    // there, so their sandboxes keep their way out.
                    temps_sandbox_providers::docker::publish_egress_recording(None);
                }
            }
            self.sleep().await;
        }
    }

    /// Binds and serves. Returns the address and a future that ends when
    /// the proxy stops.
    async fn serve(
        &self,
        gateway: IpAddr,
        service: &Arc<EgressRecordingService>,
    ) -> Result<(SocketAddr, tokio::task::JoinHandle<()>), String> {
        let preferred = SocketAddr::new(gateway, RECORDING_PROXY_PORT);
        let (proxy, listener, records) = match self.bind(preferred).await {
            Ok(bound) => bound,
            Err(error) => {
                warn!(
                    address = %preferred,
                    %error,
                    "egress recording: the recorder's port is taken; listening on a free port, \
                     so recorded sandboxes' sidecars are recreated on every restart"
                );
                self.bind(SocketAddr::new(gateway, 0)).await?
            }
        };
        let address = listener.local_addr().map_err(|e| e.to_string())?;
        tokio::spawn(EgressBatcher::default().run(
            records,
            Arc::new(LocalRecordSink::new(service.clone())),
            proxy.stats().clone(),
        ));
        let serving = tokio::spawn(async move {
            if let Err(error) = proxy.serve(listener).await {
                warn!(%error, "egress recording proxy stopped accepting");
            }
        });
        Ok((address, serving))
    }

    /// A proxy bound to `address`, its listener and its records.
    async fn bind(
        &self,
        address: SocketAddr,
    ) -> Result<
        (
            Arc<EgressProxy>,
            tokio::net::TcpListener,
            temps_workload_netpol::egress::record::EgressRecordStream,
        ),
        String,
    > {
        let (recorder, records) =
            recording_channel(temps_workload_netpol::egress::record::RECORD_CHANNEL_CAPACITY);
        let proxy = EgressProxy::with_recorder(
            EgressProxyConfig::new(address),
            self.table.clone(),
            Some(recorder),
        )
        .map_err(|e| e.to_string())?;
        let listener = proxy.bind().await.map_err(|e| e.to_string())?;
        Ok((proxy, listener, records))
    }

    async fn refresh_loop(&self, sidecars: Option<&dyn SidecarRefresh>) {
        loop {
            tokio::select! {
                _ = tokio::time::sleep(REFRESH_INTERVAL) => {}
                _ = self.wake.notified() => {}
            }
            self.refresh().await;
            if let Some(sidecars) = sidecars {
                self.retry_pending(sidecars).await;
            }
        }
    }

    /// Notes that `sandbox`'s sidecar could not be refreshed, or that it
    /// now has been.
    fn mark_pending(&self, sandbox: i32, failed: bool) {
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        if failed {
            pending.insert(sandbox);
        } else {
            pending.remove(&sandbox);
        }
    }

    /// Refreshes again the sidecars whose refresh failed.
    async fn retry_pending(&self, sidecars: &dyn SidecarRefresh) {
        let ids: Vec<i32> = self
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .copied()
            .collect();
        if ids.is_empty() {
            return;
        }
        let rows = match sandboxes::Entity::find()
            .filter(sandboxes::Column::Id.is_in(ids.clone()))
            .all(self.db.as_ref())
            .await
        {
            Ok(rows) => rows,
            Err(error) => {
                warn!(%error, "egress recording: could not read sandboxes to retry");
                return;
            }
        };
        for id in &ids {
            let Some(row) = rows.iter().find(|row| row.id == *id) else {
                self.mark_pending(*id, false);
                continue;
            };
            if row.node_id.is_some() || !(row.status == "running" || row.status == "stopped") {
                self.mark_pending(*id, false);
                continue;
            }
            match sidecars.refresh(row).await {
                Ok(()) => {
                    info!(sandbox = %row.public_id, "egress recording: the sandbox's egress proxy is refreshed");
                    self.mark_pending(*id, false);
                }
                Err(error) => {
                    warn!(sandbox = %row.public_id, %error, "egress recording: still could not refresh the sandbox's egress proxy; retrying");
                }
            }
        }
    }

    async fn sleep(&self) {
        tokio::time::sleep(REFRESH_INTERVAL).await;
    }

    /// Rebuilds the table and the published set of recorded sandboxes from
    /// the opted-in rows of this host and the running sidecars.
    pub async fn refresh(&self) {
        let Some(address) = *self.address.lock().unwrap_or_else(|e| e.into_inner()) else {
            return;
        };
        let rows = match self.recorded_rows().await {
            Ok(rows) => rows,
            Err(error) => {
                warn!(%error, "egress recording: could not read the recorded sandboxes");
                return;
            }
        };
        let names: HashSet<String> = rows.iter().map(|r| container_name(&r.public_id)).collect();
        temps_sandbox_providers::docker::publish_egress_recording(Some((address, names)));
        if rows.is_empty() {
            self.table.store(Arc::new(EgressTable::default()));
            return;
        }
        let endpoints =
            match temps_sandbox_providers::docker::egress_proxy_endpoints(&self.docker).await {
                Ok(endpoints) => endpoints,
                Err(error) => {
                    warn!(%error, "egress recording: could not list the sandbox egress proxies");
                    return;
                }
            };
        let mut identities = IdentityMap::new();
        let mut policies = Vec::with_capacity(rows.len());
        for row in &rows {
            let subject = WorkloadRef::new(WorkloadKind::Sandbox, i64::from(row.id));
            let name = container_name(&row.public_id);
            for (_, address) in endpoints.iter().filter(|(sandbox, _)| *sandbox == name) {
                identities.insert(subject, *address);
            }
            policies.push(policy::proxy_policy(subject, &row.egress_policy.0));
        }
        let table = EgressTable::build(&policies, &identities);
        debug!(
            recorded = rows.len(),
            sources = table.len(),
            "egress recording table refreshed"
        );
        self.table.store(Arc::new(table));
    }

    async fn recorded_rows(&self) -> Result<Vec<sandboxes::Model>, sea_orm::DbErr> {
        sandboxes::Entity::find()
            .filter(sandboxes::Column::EgressRecording.eq(true))
            .filter(sandboxes::Column::ProjectId.is_not_null())
            .filter(sandboxes::Column::NodeId.is_null())
            .filter(sandboxes::Column::Status.ne("destroyed"))
            .all(self.db.as_ref())
            .await
    }

    /// Once the recorder serves, bring recorded sandboxes' sidecars to its
    /// address. With the port stable this changes nothing after a restart;
    /// it matters when recording was turned on while the recorder was down,
    /// or when the port had to change. Recreation happens only on drift,
    /// and a failure is retried on every refresh.
    async fn refresh_recorded_sidecars(&self, sidecars: &dyn SidecarRefresh) {
        let rows = match self.recorded_rows().await {
            Ok(rows) => rows,
            Err(error) => {
                warn!(%error, "egress recording: could not read the recorded sandboxes");
                return;
            }
        };
        for row in rows
            .iter()
            .filter(|r| r.status == "running" || r.status == "stopped")
        {
            let result = sidecars.refresh(row).await;
            if let Err(error) = &result {
                warn!(sandbox = %row.public_id, %error, "egress recording: could not refresh the sandbox's egress proxy; retrying");
            }
            self.mark_pending(row.id, result.is_err());
        }
        self.wake.notify_one();
    }
}

/// What turning recording on or off does on this host: the table and the
/// published set follow at once, and a sandbox of this host gets its
/// sidecar recreated with or without the hand-off.
pub struct LocalHooks {
    pub recorder: Arc<LocalEgressRecorder>,
    pub sidecars: Arc<dyn SidecarRefresh>,
}

#[async_trait]
impl EgressRecordingHooks for LocalHooks {
    async fn changed(&self, row: &sandboxes::Model) {
        self.recorder.refresh().await;
        if row.node_id.is_some() || !(row.status == "running" || row.status == "stopped") {
            return;
        }
        let result = self.sidecars.refresh(row).await;
        if let Err(error) = &result {
            warn!(
                sandbox = %row.public_id,
                %error,
                "egress recording changed, but the sandbox's egress proxy could not be \
                 refreshed; retrying"
            );
        }
        self.recorder.mark_pending(row.id, result.is_err());
        self.recorder.wake.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::egress_recording::tests::{fixture, ON_CONTROL_PLANE, ON_WORKER};

    fn recorder(db: Arc<DatabaseConnection>) -> LocalEgressRecorder {
        LocalEgressRecorder {
            db,
            docker: bollard::Docker::connect_with_local_defaults().expect("docker client"),
            table: Arc::new(ArcSwap::from_pointee(EgressTable::default())),
            address: std::sync::Mutex::new(None),
            wake: Notify::new(),
            pending: std::sync::Mutex::new(HashSet::new()),
        }
    }

    /// Fails its first `failures` refreshes, then succeeds.
    struct FlakySidecars {
        failures: std::sync::Mutex<u32>,
        refreshed: std::sync::Mutex<Vec<String>>,
    }

    #[async_trait]
    impl SidecarRefresh for FlakySidecars {
        async fn refresh(&self, row: &sandboxes::Model) -> Result<(), String> {
            let mut failures = self.failures.lock().unwrap();
            if *failures > 0 {
                *failures -= 1;
                return Err("removal of the container is already in progress".into());
            }
            self.refreshed.lock().unwrap().push(row.public_id.clone());
            Ok(())
        }
    }

    /// A sidecar whose refresh failed is refreshed again until it works,
    /// so a failed replacement does not leave the sandbox without a way
    /// out; a sandbox no longer on this host is dropped from the retries.
    #[tokio::test]
    async fn a_failed_sidecar_refresh_is_retried_until_it_works() {
        let Some(f) = fixture().await else { return };
        let recorder = recorder(f.db.connection_arc());
        let sidecars = FlakySidecars {
            failures: std::sync::Mutex::new(1),
            refreshed: std::sync::Mutex::new(Vec::new()),
        };
        recorder.mark_pending(ON_CONTROL_PLANE.0, true);
        recorder.mark_pending(ON_WORKER.0, true);

        recorder.retry_pending(&sidecars).await;
        assert_eq!(
            *recorder.pending.lock().unwrap(),
            HashSet::from([ON_CONTROL_PLANE.0]),
            "the failure stays pending; the worker's sandbox is not this host's"
        );
        recorder.retry_pending(&sidecars).await;
        assert!(recorder.pending.lock().unwrap().is_empty());
        assert_eq!(
            *sidecars.refreshed.lock().unwrap(),
            vec![ON_CONTROL_PLANE.1.to_string()]
        );
    }

    /// The recorder listens on its own port, so a restart keeps the address
    /// recorded sidecars carry; only when that port is taken does it take a
    /// free one.
    #[tokio::test]
    async fn the_recorder_keeps_its_port_unless_it_is_taken() {
        let Some(f) = fixture().await else { return };
        let (service, _) = f.service();
        let recorder = recorder(f.db.connection_arc());
        let loopback = IpAddr::from([127, 0, 0, 1]);
        let Ok(occupant) =
            std::net::TcpListener::bind(SocketAddr::new(loopback, RECORDING_PROXY_PORT))
        else {
            eprintln!("port {RECORDING_PROXY_PORT} is in use on this host; skipping");
            return;
        };
        let (taken, serving) = recorder.serve(loopback, &service).await.expect("serve");
        assert_ne!(taken.port(), RECORDING_PROXY_PORT);
        serving.abort();
        drop(occupant);

        let (free, serving) = recorder.serve(loopback, &service).await.expect("serve");
        assert_eq!(free, SocketAddr::new(loopback, RECORDING_PROXY_PORT));
        serving.abort();
    }

    #[test]
    fn container_names_follow_the_sandbox_public_id() {
        assert_eq!(
            container_name("sbx_10a5f86bd8ff859d"),
            "temps-sandbox-10a5f86bd8ff859d"
        );
    }
}
