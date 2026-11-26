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

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use async_trait::async_trait;
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter};
use temps_entities::sandboxes;
use temps_workload::{WorkloadKind, WorkloadRef};
use temps_workload_egress::local::{LocalTraffic, RecordedTraffic};
use temps_workload_netpol::IdentityMap;
use tracing::warn;

use super::policy;

/// Container-name prefix of a sandbox (`temps-sandbox-<label>`).
const SANDBOX_CONTAINER_PREFIX: &str = "temps-sandbox-";

/// Re-applies a sandbox's egress so its sidecar is recreated with or
/// without the hand-off to the recorder.
///
/// A refresh always enforces the policy stored for the sandbox *after*
/// taking [`SidecarRefresh::serialize`]: the policy update holds the same
/// claim while it applies a new policy and stores it, so a refresh can
/// never put back a policy that an update replaced in the meantime.
#[async_trait]
pub trait SidecarRefresh: Send + Sync {
    /// Waits for, then holds, the claim every change to sandbox `id`'s
    /// compute takes. Dropping the returned value releases it.
    async fn serialize(&self, id: i32) -> Box<dyn std::any::Any + Send>;

    /// Re-applies `row`'s stored policy, read under that claim.
    async fn apply(&self, row: &sandboxes::Model) -> Result<(), String>;
}

#[async_trait]
impl SidecarRefresh for crate::services::registry::StandaloneSandboxRegistry {
    async fn serialize(&self, id: i32) -> Box<dyn std::any::Any + Send> {
        Box::new(self.begin_planned_downtime(id).await)
    }

    async fn apply(&self, row: &sandboxes::Model) -> Result<(), String> {
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

/// Each sandbox's sidecar addresses on the egress network, by sandbox
/// container name: one pass over the endpoints, so matching rows against
/// them costs one lookup each instead of a scan.
fn endpoints_by_sandbox(endpoints: Vec<(String, IpAddr)>) -> HashMap<String, Vec<IpAddr>> {
    let mut by_sandbox: HashMap<String, Vec<IpAddr>> = HashMap::with_capacity(endpoints.len());
    for (sandbox, address) in endpoints {
        by_sandbox.entry(sandbox).or_default().push(address);
    }
    by_sandbox
}

/// The recorded sandboxes of this host, and their sidecars.
pub struct SandboxTraffic {
    db: Arc<DatabaseConnection>,
    docker: bollard::Docker,
    /// Absent without a local registry: the table still follows opt-ins,
    /// and sidecars pick the hand-off up when they are next reconciled.
    sidecars: Option<Arc<dyn SidecarRefresh>>,
    /// Sandboxes whose sidecar could not be refreshed: retried after every
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

    /// This host's opted-in sandboxes the recorder can see (Docker ones).
    async fn recorded_rows(&self) -> Result<Vec<sandboxes::Model>, sea_orm::DbErr> {
        Ok(sandboxes::Entity::find()
            .filter(sandboxes::Column::EgressRecording.eq(true))
            .filter(sandboxes::Column::NodeId.is_null())
            .filter(sandboxes::Column::Status.ne("destroyed"))
            .all(self.db.as_ref())
            .await?
            .into_iter()
            .filter(|row| super::records_on_backend(row.backend.as_deref()))
            .collect())
    }

    /// Replaces sandbox `id`'s sidecar so it matches the recorder's state,
    /// holding the claim a policy update holds, and with the policy stored
    /// for it once that claim is held. `Ok` also when there is nothing to
    /// do (gone, on a worker, not running or stopped).
    async fn refresh_sidecar(&self, sidecars: &dyn SidecarRefresh, id: i32) -> Result<(), String> {
        let _claim = sidecars.serialize(id).await;
        let row = sandboxes::Entity::find_by_id(id)
            .one(self.db.as_ref())
            .await
            .map_err(|e| format!("could not read sandbox {id}: {e}"))?;
        match row {
            Some(row) if steerable(&row) => sidecars.apply(&row).await,
            _ => Ok(()),
        }
    }

    /// Refreshes the sidecars of `ids`, keeping each failure for a retry.
    async fn refresh_sidecars(&self, ids: impl IntoIterator<Item = i32>, why: &str) {
        let Some(sidecars) = &self.sidecars else {
            return;
        };
        for id in ids {
            let result = self.refresh_sidecar(sidecars.as_ref(), id).await;
            if let Err(error) = &result {
                warn!(sandbox = id, %error, why, "egress recording: the sandbox's egress proxy could not be refreshed; retrying");
            }
            self.mark_pending(id, result.is_err());
        }
    }

    /// The opt-in of sandbox `id` changed: recreate its sidecar with or
    /// without the hand-off, when it is this host's and can be.
    pub async fn steer(&self, id: i32) {
        self.refresh_sidecars([id], "its recording changed").await;
    }

    /// Refreshes again the sidecars whose refresh failed.
    async fn retry_pending(&self) {
        let ids: Vec<i32> = self
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .copied()
            .collect();
        if !ids.is_empty() {
            self.refresh_sidecars(ids, "an earlier refresh failed")
                .await;
        }
    }

    /// The ids of this host's recorded sandboxes whose sidecar can be
    /// replaced now.
    async fn steerable_recorded(&self) -> Vec<i32> {
        match self.recorded_rows().await {
            Ok(rows) => rows
                .iter()
                .filter(|row| steerable(row))
                .map(|row| row.id)
                .collect(),
            Err(error) => {
                warn!(%error, "egress recording: could not read the recorded sandboxes");
                Vec::new()
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
    /// sidecar. Reads only: replacing a sidecar is [`Self::maintain`]'s.
    async fn refresh(&self, address: SocketAddr) -> Result<RecordedTraffic, String> {
        let rows = self
            .recorded_rows()
            .await
            .map_err(|e| format!("could not read the recorded sandboxes: {e}"))?;
        let names: HashSet<String> = rows.iter().map(|r| container_name(&r.public_id)).collect();
        temps_sandbox_providers::docker::publish_egress_recording(Some((address, names)));
        let mut traffic = RecordedTraffic::default();
        if rows.is_empty() {
            return Ok(traffic);
        }
        let endpoints = endpoints_by_sandbox(
            temps_sandbox_providers::docker::egress_proxy_endpoints(&self.docker)
                .await
                .map_err(|e| format!("could not list the sandbox egress proxies: {e}"))?,
        );
        let mut identities = IdentityMap::new();
        for row in &rows {
            let subject = WorkloadRef::new(WorkloadKind::Sandbox, i64::from(row.id));
            for address in endpoints
                .get(&container_name(&row.public_id))
                .into_iter()
                .flatten()
            {
                identities.insert(subject, *address);
            }
            traffic
                .policies
                .push(policy::proxy_policy(subject, &row.egress_policy.0));
        }
        traffic.identities = identities;
        Ok(traffic)
    }

    /// Retries the sidecars whose replacement failed, outside the refresh:
    /// replacing one may itself refresh the recorder.
    async fn maintain(&self) {
        self.retry_pending().await;
    }

    /// Once the recorder serves, bring recorded sandboxes' sidecars to its
    /// address. With the port fixed this changes nothing after a restart;
    /// it matters when recording was turned on while the recorder was down.
    /// Recreation happens only on drift, and a failure is retried.
    async fn started(&self, _address: SocketAddr) {
        let ids = self.steerable_recorded().await;
        self.refresh_sidecars(ids, "the recorder started").await;
    }

    /// The recorder does not serve sandboxes: running sidecars still hand
    /// traffic to its address, so they are replaced without the hand-off
    /// and their sandboxes keep their way out instead of failing every
    /// request until it is back.
    async fn stopped(&self) {
        temps_sandbox_providers::docker::publish_egress_recording(None);
        let ids = self.steerable_recorded().await;
        self.refresh_sidecars(ids, "the recorder stopped").await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::egress_recording::tests::{fixture, ON_CONTROL_PLANE, ON_WORKER};
    use sea_orm::{ActiveModelTrait, ConnectionTrait, Set};
    use temps_sandbox_providers::{EgressMode, SandboxEgressPolicy};

    /// Fails its first `failures` applies, then succeeds; records which
    /// sandbox got which policy mode. Its claim is one lock for all.
    struct FlakySidecars {
        failures: std::sync::Mutex<u32>,
        applied: std::sync::Mutex<Vec<(String, EgressMode)>>,
        claim: Arc<tokio::sync::Mutex<()>>,
    }

    impl FlakySidecars {
        fn failing(failures: u32) -> Arc<Self> {
            Arc::new(Self {
                failures: std::sync::Mutex::new(failures),
                applied: std::sync::Mutex::new(Vec::new()),
                claim: Arc::new(tokio::sync::Mutex::new(())),
            })
        }

        fn applied_to(&self) -> Vec<String> {
            self.applied
                .lock()
                .unwrap()
                .iter()
                .map(|(id, _)| id.clone())
                .collect()
        }
    }

    #[async_trait]
    impl SidecarRefresh for FlakySidecars {
        async fn serialize(&self, _id: i32) -> Box<dyn std::any::Any + Send> {
            Box::new(self.claim.clone().lock_owned().await)
        }

        async fn apply(&self, row: &sandboxes::Model) -> Result<(), String> {
            let mut failures = self.failures.lock().unwrap();
            if *failures > 0 {
                *failures -= 1;
                return Err("removal of the container is already in progress".into());
            }
            self.applied
                .lock()
                .unwrap()
                .push((row.public_id.clone(), row.egress_policy.0.mode));
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

        traffic.retry_pending().await;
        assert_eq!(
            *traffic.pending.lock().unwrap(),
            HashSet::from([ON_CONTROL_PLANE.0]),
            "the failure stays pending; the worker's sandbox is not this host's"
        );
        traffic.retry_pending().await;
        assert!(traffic.pending.lock().unwrap().is_empty());
        assert_eq!(sidecars.applied_to(), vec![ON_CONTROL_PLANE.1.to_string()]);
    }

    /// A changed opt-in replaces the sidecar of a sandbox this host runs,
    /// leaves a worker's alone, and keeps a failure for the next refresh.
    #[tokio::test]
    async fn a_changed_opt_in_steers_only_this_hosts_sandboxes() {
        let Some(f) = fixture().await else { return };
        let sidecars = FlakySidecars::failing(1);
        let traffic = traffic(f.db.connection_arc(), sidecars.clone());

        traffic.steer(ON_WORKER.0).await;
        assert!(sidecars.applied_to().is_empty());
        assert!(traffic.pending.lock().unwrap().is_empty());

        traffic.steer(ON_CONTROL_PLANE.0).await;
        assert_eq!(
            *traffic.pending.lock().unwrap(),
            HashSet::from([ON_CONTROL_PLANE.0]),
            "the first refresh failed"
        );
        traffic.steer(ON_CONTROL_PLANE.0).await;
        assert!(traffic.pending.lock().unwrap().is_empty());
        assert_eq!(sidecars.applied_to(), vec![ON_CONTROL_PLANE.1.to_string()]);
    }

    /// Steering waits for a policy update in progress and then applies the
    /// policy that update stored, never the one it read before: turning
    /// recording off while the sandbox is being restricted must not put
    /// internet access back.
    #[tokio::test]
    async fn steering_applies_the_policy_stored_after_a_concurrent_update() {
        let Some(f) = fixture().await else { return };
        let sidecars = FlakySidecars::failing(0);
        let traffic = Arc::new(traffic(f.db.connection_arc(), sidecars.clone()));

        // A policy update holds the claim while it applies and stores.
        let update = sidecars.claim.clone().lock_owned().await;
        let steering = {
            let traffic = traffic.clone();
            tokio::spawn(async move { traffic.steer(ON_CONTROL_PLANE.0).await })
        };
        tokio::task::yield_now().await;
        sandboxes::ActiveModel {
            id: Set(ON_CONTROL_PLANE.0),
            egress_policy: Set(sandboxes::StoredEgressPolicy(SandboxEgressPolicy {
                mode: EgressMode::Restricted,
                rules: Vec::new(),
            })),
            ..Default::default()
        }
        .update(f.db.connection())
        .await
        .unwrap();
        drop(update);
        steering.await.unwrap();

        assert_eq!(
            *sidecars.applied.lock().unwrap(),
            vec![(ON_CONTROL_PLANE.1.to_string(), EgressMode::Restricted)]
        );
    }

    /// When the recorder stops serving sandboxes, every recorded sidecar of
    /// this host is replaced without the hand-off, so its sandbox keeps its
    /// way out instead of failing every request while the recorder is down.
    #[tokio::test]
    async fn a_stopped_recorder_releases_the_sidecars_handing_traffic_to_it() {
        let Some(f) = fixture().await else { return };
        f.db.connection()
            .execute_unprepared(&format!(
                "UPDATE sandboxes SET egress_recording = true WHERE id IN ({}, {})",
                ON_CONTROL_PLANE.0, ON_WORKER.0
            ))
            .await
            .unwrap();
        let sidecars = FlakySidecars::failing(0);
        let traffic = traffic(f.db.connection_arc(), sidecars.clone());

        traffic.stopped().await;
        assert_eq!(
            sidecars.applied_to(),
            vec![ON_CONTROL_PLANE.1.to_string()],
            "only this host's sandboxes have a sidecar here"
        );
    }

    /// Sidecars are found by name in one pass, whatever the number of
    /// recorded sandboxes.
    #[test]
    fn endpoints_are_indexed_by_sandbox() {
        let address = |a: &str| a.parse::<IpAddr>().unwrap();
        let index = endpoints_by_sandbox(vec![
            ("temps-sandbox-a".into(), address("172.30.0.2")),
            ("temps-sandbox-b".into(), address("172.30.0.3")),
            ("temps-sandbox-a".into(), address("172.30.0.4")),
        ]);
        assert_eq!(
            index["temps-sandbox-a"],
            vec![address("172.30.0.2"), address("172.30.0.4")]
        );
        assert_eq!(index["temps-sandbox-b"], vec![address("172.30.0.3")]);
        assert!(!index.contains_key("temps-sandbox-c"));
    }

    #[test]
    fn container_names_follow_the_sandbox_public_id() {
        assert_eq!(
            container_name("sbx_10a5f86bd8ff859d"),
            "temps-sandbox-10a5f86bd8ff859d"
        );
    }
}
