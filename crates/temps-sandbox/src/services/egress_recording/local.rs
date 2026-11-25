// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! How a sandbox's traffic reaches the control plane's egress recorder.
//!
//! The recorder itself (one proxy, one table, for every workload kind) is
//! `temps_workload_egress::local`. This is the sandbox kind's
//! [`LocalTraffic`]:
//!
//! **Only opted-in sandboxes go through the recorder.** Every other
//! sandbox keeps egressing through its own sidecar exactly as before. A
//! recorded sandbox's sidecar keeps the Temps relays and internal names
//! (`*.temps.local`), and hands every other outbound request to the
//! recorder (`TEMPS_EGRESS_RECORDING_PROXY_ADDRESS`), which applies the
//! sandbox's own egress policy ([`super::policy`]) and records each
//! connection, allowed or refused.
//!
//! **Where the recorder listens**: the host's address on the sidecars'
//! shared egress network, which is created with the first sandbox.
//!
//! **Who is calling** comes from the source address: a sidecar reaches the
//! recorder from its own address on that network, which only Docker
//! assigns, and that address speaks for the sandbox the sidecar serves
//! (`egress_proxy_sandbox`, the same Docker-recorded identity the internal
//! proxy trusts, ADR 105).
//!
//! **Cost.** Each refresh is one query for the opted-in sandboxes of this
//! host; only when there are some, one local container list.

use std::collections::HashSet;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use async_trait::async_trait;
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter};
use temps_entities::sandboxes;
use temps_workload::{WorkloadKind, WorkloadRef};
use temps_workload_egress::local::{LocalTraffic, RecordedTraffic};
use temps_workload_netpol::IdentityMap;
use tracing::{info, warn};

use super::policy;

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

/// The sandbox container name of a public id.
pub fn container_name(public_id: &str) -> String {
    format!(
        "{SANDBOX_CONTAINER_PREFIX}{}",
        public_id.strip_prefix("sbx_").unwrap_or(public_id)
    )
}

/// A sandbox whose sidecar this host manages and can replace now.
fn steerable(row: &sandboxes::Model) -> bool {
    row.node_id.is_none() && (row.status == "running" || row.status == "stopped")
}

/// The recorded sandboxes of this host, and their sidecars.
pub struct SandboxTraffic {
    db: Arc<DatabaseConnection>,
    docker: bollard::Docker,
    /// Absent without a local registry: the table still follows opt-ins,
    /// and sidecars pick the hand-off up when they are next reconciled.
    sidecars: Option<Arc<dyn SidecarRefresh>>,
    /// Sandboxes whose sidecar could not be refreshed: retried on every
    /// refresh until it works, so a failed replacement never leaves a
    /// sandbox without a way out for long.
    pending: std::sync::Mutex<HashSet<i32>>,
}

impl SandboxTraffic {
    pub fn new(
        db: Arc<DatabaseConnection>,
        docker: bollard::Docker,
        sidecars: Option<Arc<dyn SidecarRefresh>>,
    ) -> Self {
        Self {
            db,
            docker,
            sidecars,
            pending: std::sync::Mutex::new(HashSet::new()),
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

    async fn recorded_rows(&self) -> Result<Vec<sandboxes::Model>, sea_orm::DbErr> {
        sandboxes::Entity::find()
            .filter(sandboxes::Column::EgressRecording.eq(true))
            .filter(sandboxes::Column::NodeId.is_null())
            .filter(sandboxes::Column::Status.ne("destroyed"))
            .all(self.db.as_ref())
            .await
    }

    /// The opt-in of sandbox `id` changed: recreate its sidecar with or
    /// without the hand-off, when it is this host's and can be.
    pub async fn steer(&self, id: i32) {
        let Some(sidecars) = &self.sidecars else {
            return;
        };
        let row = match sandboxes::Entity::find_by_id(id)
            .one(self.db.as_ref())
            .await
        {
            Ok(Some(row)) => row,
            Ok(None) => return,
            Err(error) => {
                warn!(sandbox = id, %error, "egress recording changed, but the sandbox could not be read; retrying");
                self.mark_pending(id, true);
                return;
            }
        };
        if !steerable(&row) {
            return;
        }
        let result = sidecars.refresh(&row).await;
        if let Err(error) = &result {
            warn!(
                sandbox = %row.public_id,
                %error,
                "egress recording changed, but the sandbox's egress proxy could not be \
                 refreshed; retrying"
            );
        }
        self.mark_pending(row.id, result.is_err());
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
            if !steerable(row) {
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
}

#[async_trait]
impl LocalTraffic for SandboxTraffic {
    fn kind(&self) -> WorkloadKind {
        WorkloadKind::Sandbox
    }

    /// The host's address on the sidecars' egress network, once the first
    /// sandbox created it.
    async fn listen_address(&self) -> Result<Option<IpAddr>, String> {
        temps_sandbox_providers::docker::egress_network_gateway(&self.docker)
            .await
            .map_err(|e| {
                format!(
                    "could not inspect the sandbox egress network '{}': {e}",
                    temps_sandbox_providers::docker::SANDBOX_EGRESS_NETWORK
                )
            })
    }

    /// Publishes the recorder's address and the recorded sandboxes for the
    /// sidecars, and returns each one's policy and the addresses of its
    /// sidecar.
    async fn refresh(&self, address: SocketAddr) -> Result<RecordedTraffic, String> {
        let rows = self
            .recorded_rows()
            .await
            .map_err(|e| format!("could not read the recorded sandboxes: {e}"))?;
        let names: HashSet<String> = rows.iter().map(|r| container_name(&r.public_id)).collect();
        temps_sandbox_providers::docker::publish_egress_recording(Some((address, names)));
        let mut traffic = RecordedTraffic::default();
        if !rows.is_empty() {
            let endpoints = temps_sandbox_providers::docker::egress_proxy_endpoints(&self.docker)
                .await
                .map_err(|e| format!("could not list the sandbox egress proxies: {e}"))?;
            let mut identities = IdentityMap::new();
            for row in &rows {
                let subject = WorkloadRef::new(WorkloadKind::Sandbox, i64::from(row.id));
                let name = container_name(&row.public_id);
                for (_, address) in endpoints.iter().filter(|(sandbox, _)| *sandbox == name) {
                    identities.insert(subject, *address);
                }
                traffic
                    .policies
                    .push(policy::proxy_policy(subject, &row.egress_policy.0));
            }
            traffic.identities = identities;
        }
        if let Some(sidecars) = &self.sidecars {
            self.retry_pending(sidecars.as_ref()).await;
        }
        Ok(traffic)
    }

    /// Once the recorder serves, bring recorded sandboxes' sidecars to its
    /// address. With the port stable this changes nothing after a restart;
    /// it matters when recording was turned on while the recorder was down,
    /// or when the port had to change. Recreation happens only on drift,
    /// and a failure is retried on every refresh.
    async fn started(&self, _address: SocketAddr) {
        let Some(sidecars) = &self.sidecars else {
            return;
        };
        let rows = match self.recorded_rows().await {
            Ok(rows) => rows,
            Err(error) => {
                warn!(%error, "egress recording: could not read the recorded sandboxes");
                return;
            }
        };
        for row in rows.iter().filter(|row| steerable(row)) {
            let result = sidecars.refresh(row).await;
            if let Err(error) = &result {
                warn!(sandbox = %row.public_id, %error, "egress recording: could not refresh the sandbox's egress proxy; retrying");
            }
            self.mark_pending(row.id, result.is_err());
        }
    }

    /// Sidecars stop handing traffic to a recorder that is not there, so
    /// their sandboxes keep their way out.
    async fn stopped(&self) {
        temps_sandbox_providers::docker::publish_egress_recording(None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::egress_recording::tests::{fixture, ON_CONTROL_PLANE, ON_WORKER};

    /// Fails its first `failures` refreshes, then succeeds.
    struct FlakySidecars {
        failures: std::sync::Mutex<u32>,
        refreshed: std::sync::Mutex<Vec<String>>,
    }

    impl FlakySidecars {
        fn failing(failures: u32) -> Arc<Self> {
            Arc::new(Self {
                failures: std::sync::Mutex::new(failures),
                refreshed: std::sync::Mutex::new(Vec::new()),
            })
        }
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

    fn traffic(db: Arc<DatabaseConnection>, sidecars: Arc<FlakySidecars>) -> SandboxTraffic {
        SandboxTraffic::new(
            db,
            bollard::Docker::connect_with_local_defaults().expect("docker client"),
            Some(sidecars),
        )
    }

    /// A sidecar whose refresh failed is refreshed again until it works,
    /// so a failed replacement does not leave the sandbox without a way
    /// out; a sandbox no longer on this host is dropped from the retries.
    #[tokio::test]
    async fn a_failed_sidecar_refresh_is_retried_until_it_works() {
        let Some(f) = fixture().await else { return };
        let sidecars = FlakySidecars::failing(1);
        let traffic = traffic(f.db.connection_arc(), sidecars.clone());
        traffic.mark_pending(ON_CONTROL_PLANE.0, true);
        traffic.mark_pending(ON_WORKER.0, true);

        traffic.retry_pending(sidecars.as_ref()).await;
        assert_eq!(
            *traffic.pending.lock().unwrap(),
            HashSet::from([ON_CONTROL_PLANE.0]),
            "the failure stays pending; the worker's sandbox is not this host's"
        );
        traffic.retry_pending(sidecars.as_ref()).await;
        assert!(traffic.pending.lock().unwrap().is_empty());
        assert_eq!(
            *sidecars.refreshed.lock().unwrap(),
            vec![ON_CONTROL_PLANE.1.to_string()]
        );
    }

    /// A changed opt-in replaces the sidecar of a sandbox this host runs,
    /// leaves a worker's alone, and keeps a failure for the next refresh.
    #[tokio::test]
    async fn a_changed_opt_in_steers_only_this_hosts_sandboxes() {
        let Some(f) = fixture().await else { return };
        let sidecars = FlakySidecars::failing(1);
        let traffic = traffic(f.db.connection_arc(), sidecars.clone());

        traffic.steer(ON_WORKER.0).await;
        assert!(sidecars.refreshed.lock().unwrap().is_empty());
        assert!(traffic.pending.lock().unwrap().is_empty());

        traffic.steer(ON_CONTROL_PLANE.0).await;
        assert_eq!(
            *traffic.pending.lock().unwrap(),
            HashSet::from([ON_CONTROL_PLANE.0]),
            "the first refresh failed"
        );
        traffic.steer(ON_CONTROL_PLANE.0).await;
        assert!(traffic.pending.lock().unwrap().is_empty());
        assert_eq!(
            *sidecars.refreshed.lock().unwrap(),
            vec![ON_CONTROL_PLANE.1.to_string()]
        );
    }

    #[test]
    fn container_names_follow_the_sandbox_public_id() {
        assert_eq!(
            container_name("sbx_10a5f86bd8ff859d"),
            "temps-sandbox-10a5f86bd8ff859d"
        );
    }
}
