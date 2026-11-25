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
        });
        if !cfg!(target_os = "linux") {
            info!(
                os = std::env::consts::OS,
                "egress recording: the control plane records only on Linux"
            );
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
                        _ = self.refresh_loop() => {}
                    }
                    warn!(%address, "egress recording proxy stopped; restarting it");
                    capability.set(EgressRecordingCapability::detect(false));
                    *self.address.lock().unwrap_or_else(|e| e.into_inner()) = None;
                    temps_sandbox_providers::docker::publish_egress_recording(None);
                }
                Err(error) => {
                    warn!(%error, "egress recording proxy could not start");
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
        let (recorder, records) =
            recording_channel(temps_workload_netpol::egress::record::RECORD_CHANNEL_CAPACITY);
        let proxy = EgressProxy::with_recorder(
            EgressProxyConfig::new(SocketAddr::new(gateway, 0)),
            self.table.clone(),
            Some(recorder),
        )
        .map_err(|e| e.to_string())?;
        let listener = proxy.bind().await.map_err(|e| e.to_string())?;
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

    async fn refresh_loop(&self) {
        loop {
            tokio::select! {
                _ = tokio::time::sleep(REFRESH_INTERVAL) => {}
                _ = self.wake.notified() => {}
            }
            self.refresh().await;
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

    /// After a restart the recorder listens on a new port: recreate the
    /// sidecars of recorded sandboxes whose hand-off names another one.
    /// Recreation happens only on drift, so unchanged sidecars are kept.
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
            if let Err(error) = sidecars.refresh(row).await {
                warn!(sandbox = %row.public_id, %error, "egress recording: could not refresh the sandbox's egress proxy");
            }
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
        if let Err(error) = self.sidecars.refresh(row).await {
            warn!(
                sandbox = %row.public_id,
                %error,
                "egress recording changed, but the sandbox's egress proxy could not be refreshed; \
                 it applies on the sandbox's next start"
            );
        }
        self.recorder.wake.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn container_names_follow_the_sandbox_public_id() {
        assert_eq!(
            container_name("sbx_10a5f86bd8ff859d"),
            "temps-sandbox-10a5f86bd8ff859d"
        );
    }
}
