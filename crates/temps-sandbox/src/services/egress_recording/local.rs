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
use sea_orm::{
    ColumnTrait, Condition, ConnectionTrait, DatabaseBackend, DatabaseConnection, EntityTrait,
    QueryFilter, QuerySelect, Statement,
};
use temps_entities::sandboxes;
use temps_sandbox_providers::docker::EgressProxyEndpoint;
use temps_workload::{WorkloadKind, WorkloadRef};
use temps_workload_egress::local::{LocalTraffic, LocalTrafficError, RecordedTraffic};
use temps_workload_egress::steering::InstallationId;
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

/// The container of `row` as the Docker provider names it: after its agent
/// run for a sandbox an agent run started, after its public id otherwise.
/// Its sidecar is named after it, so a name built from the wrong one never
/// matches and the sandbox would record nothing.
pub fn row_container_name(row: &sandboxes::Model) -> String {
    container_of(&row.public_id, row.agent_run_id)
}

/// [`row_container_name`] from the two columns it reads.
pub fn container_of(public_id: &str, agent_run_id: Option<i32>) -> String {
    match agent_run_id {
        Some(run_id) => format!("{SANDBOX_CONTAINER_PREFIX}{run_id}"),
        None => container_name(public_id),
    }
}

/// A sandbox whose sidecar this host manages and can replace now.
fn steerable(row: &sandboxes::Model) -> bool {
    row.node_id.is_none() && steerable_status(&row.status)
}

fn steerable_status(status: &str) -> bool {
    status == "running" || status == "stopped"
}

/// What the recorder reads of a recorded sandbox of this host: the columns
/// its table and its sidecars need, never the rest of the row (metadata,
/// preview credentials) it would otherwise keep cached.
#[derive(Debug, Clone, PartialEq, sea_orm::FromQueryResult)]
pub struct RecordedRow {
    pub id: i32,
    pub public_id: String,
    pub agent_run_id: Option<i32>,
    pub status: String,
    pub egress_policy: sandboxes::StoredEgressPolicy,
}

impl RecordedRow {
    pub fn container_name(&self) -> String {
        container_of(&self.public_id, self.agent_run_id)
    }
}

/// The sidecars among `endpoints` that hand their traffic to this
/// installation's recorder. Two installations can share a Docker daemon
/// (and, after a takeover, the recorder's listener): a sidecar another one
/// started is never taken for one of this installation's sandboxes, whose
/// container name can be the same, nor replaced. A sidecar that does not
/// hand off sends nothing here, and one labelled by an earlier version
/// (the address alone) names nobody: a recorded sandbox's is recreated
/// with this installation's label when the recorder starts.
fn handing_off_here(
    endpoints: Vec<EgressProxyEndpoint>,
    installation: InstallationId,
) -> Vec<EgressProxyEndpoint> {
    let ours = installation.to_string();
    endpoints
        .into_iter()
        .filter(|endpoint| endpoint.hands_off && endpoint.installation.as_deref() == Some(&ours))
        .collect()
}

/// Each sandbox's sidecar addresses on the egress network, by sandbox
/// container name: one pass over the endpoints, so matching rows against
/// them costs one lookup each instead of a scan.
fn endpoints_by_sandbox(endpoints: &[EgressProxyEndpoint]) -> HashMap<&str, Vec<IpAddr>> {
    let mut by_sandbox: HashMap<&str, Vec<IpAddr>> = HashMap::with_capacity(endpoints.len());
    for endpoint in endpoints {
        by_sandbox
            .entry(endpoint.sandbox.as_str())
            .or_default()
            .push(endpoint.address);
    }
    by_sandbox
}

/// What [`SandboxTraffic::stray_ids`] reads of a sandbox that may have a
/// stray hand-off: enough to name its container and tell whether its
/// sidecar can be replaced, never its policy.
#[derive(Debug, Clone, PartialEq, sea_orm::FromQueryResult)]
pub struct StrayCandidate {
    pub id: i32,
    pub public_id: String,
    pub agent_run_id: Option<i32>,
    pub status: String,
}

impl StrayCandidate {
    fn container_name(&self) -> String {
        container_of(&self.public_id, self.agent_run_id)
    }
}

/// The rows a sidecar of sandbox container `name` can belong to: the
/// public ids and agent run ids [`container_of`] could have built it from.
/// None for a name it never builds.
fn container_sources(name: &str) -> Option<(Vec<String>, Option<i32>)> {
    let suffix = name.strip_prefix(SANDBOX_CONTAINER_PREFIX)?;
    if suffix.is_empty() {
        return None;
    }
    Some((
        vec![format!("sbx_{suffix}"), suffix.to_string()],
        suffix.parse::<i32>().ok(),
    ))
}

/// The sandboxes among `rows` (which do not record) whose sidecar is listed
/// handing its traffic to the recorder, and that can be replaced now.
fn strays_among(rows: &[StrayCandidate], endpoints: &[EgressProxyEndpoint]) -> Vec<i32> {
    let handing_off: HashSet<&str> = endpoints
        .iter()
        .filter(|endpoint| endpoint.hands_off)
        .map(|endpoint| endpoint.sandbox.as_str())
        .collect();
    rows.iter()
        .filter(|row| steerable_status(&row.status))
        .filter(|row| handing_off.contains(row.container_name().as_str()))
        .map(|row| row.id)
        .collect()
}

/// Who a stray hand-off speaks for in the table: no sandbox (ids start at
/// 1), and never recorded.
pub const STRAY: WorkloadRef = WorkloadRef {
    kind: WorkloadKind::Sandbox,
    id: 0,
};

/// The table entries for the recorded `rows` and the sidecars listed, and
/// whether any listed sidecar hands its traffic to the recorder for a
/// sandbox that no longer records. Those are let through unrecorded
/// ([`policy::stray_policy`]): an opt-out the sidecar has not followed yet
/// (the control plane stopped between storing it and replacing the
/// sidecar) must not leave the sandbox refused as unknown.
fn traffic_from(
    rows: &[RecordedRow],
    endpoints: &[EgressProxyEndpoint],
) -> (RecordedTraffic, bool) {
    let by_sandbox = endpoints_by_sandbox(endpoints);
    let mut traffic = RecordedTraffic::default();
    let mut recorded: HashSet<String> = HashSet::with_capacity(rows.len());
    for row in rows {
        let name = row.container_name();
        let subject = WorkloadRef::new(WorkloadKind::Sandbox, i64::from(row.id));
        for address in by_sandbox.get(name.as_str()).into_iter().flatten() {
            traffic.identities.insert(subject, *address);
        }
        traffic
            .policies
            .push(policy::proxy_policy(subject, &row.egress_policy.0));
        recorded.insert(name);
    }
    let mut strays = false;
    for endpoint in endpoints {
        if endpoint.hands_off && !recorded.contains(&endpoint.sandbox) {
            traffic.identities.insert(STRAY, endpoint.address);
            strays = true;
        }
    }
    if strays {
        traffic.policies.push(policy::stray_policy(STRAY));
    }
    (traffic, strays)
}

/// The recorded sandboxes of this host, and their sidecars.
pub struct SandboxTraffic {
    db: Arc<DatabaseConnection>,
    docker: bollard::Docker,
    /// This installation, as its sidecars' labels name it.
    installation: InstallationId,
    /// Absent without a local registry: the table still follows opt-ins,
    /// and sidecars pick the hand-off up when they are next reconciled.
    sidecars: Option<Arc<dyn SidecarRefresh>>,
    /// Sandboxes whose sidecar could not be refreshed: retried after every
    /// refresh until it works, so a failed replacement never leaves a
    /// sandbox without a way out for long.
    pending: std::sync::Mutex<HashSet<i32>>,
    /// Set when the recorder started or stopped but the sandboxes to
    /// re-point could not be read: every recorded sidecar is refreshed on
    /// the next [`LocalTraffic::maintain`] until the read works.
    resync: std::sync::atomic::AtomicBool,
    /// Whether the recorded set the sidecars read was published from the
    /// database as it is: false after a refresh could not read it. A
    /// sidecar replaced meanwhile would get the old hand-off (or none) and
    /// keep it, so replacements wait, pending, until a refresh publishes.
    published: std::sync::atomic::AtomicBool,
    /// The recorded rows, and the network policy revision they were read
    /// at. Reused while the revision is unchanged: it moves whenever a
    /// sandbox is created, removed, or changes status, opt-in, node or id.
    /// A policy change does not move it, so it reloads by discovering.
    rows: std::sync::Mutex<Option<(i64, Arc<Vec<RecordedRow>>)>>,
    /// Whether the last list found a sidecar handing off for a sandbox that
    /// no longer records. Starts true: a restart does not know.
    strays_seen: std::sync::atomic::AtomicBool,
}

impl SandboxTraffic {
    pub fn new(
        db: Arc<DatabaseConnection>,
        docker: bollard::Docker,
        installation: InstallationId,
        sidecars: Option<Arc<dyn SidecarRefresh>>,
    ) -> Self {
        Self {
            db,
            docker,
            installation,
            sidecars,
            pending: std::sync::Mutex::new(HashSet::new()),
            resync: std::sync::atomic::AtomicBool::new(false),
            published: std::sync::atomic::AtomicBool::new(false),
            rows: std::sync::Mutex::new(None),
            strays_seen: std::sync::atomic::AtomicBool::new(true),
        }
    }

    /// This host's sidecars that hand their traffic to this installation's
    /// recorder ([`handing_off_here`]).
    async fn endpoints(&self) -> Result<Vec<EgressProxyEndpoint>, bollard::errors::Error> {
        temps_sandbox_providers::docker::egress_proxy_endpoints(&self.docker)
            .await
            .map(|endpoints| handing_off_here(endpoints, self.installation))
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

    /// This host's opted-in sandboxes the recorder can see (Docker ones),
    /// as much of each row as the recorder uses.
    async fn recorded_rows(&self) -> Result<Vec<RecordedRow>, sea_orm::DbErr> {
        self.local_rows(true).await
    }

    /// This host's live Docker sandboxes that record (`recording`) or not.
    async fn local_rows(&self, recording: bool) -> Result<Vec<RecordedRow>, sea_orm::DbErr> {
        sandboxes::Entity::find()
            .select_only()
            .columns([
                sandboxes::Column::Id,
                sandboxes::Column::PublicId,
                sandboxes::Column::AgentRunId,
                sandboxes::Column::Status,
                sandboxes::Column::EgressPolicy,
            ])
            .filter(sandboxes::Column::EgressRecording.eq(recording))
            .filter(sandboxes::Column::NodeId.is_null())
            .filter(sandboxes::Column::Status.ne("destroyed"))
            .filter(
                Condition::any()
                    .add(sandboxes::Column::Backend.is_null())
                    .add(sandboxes::Column::Backend.eq("docker")),
            )
            .into_model::<RecordedRow>()
            .all(self.db.as_ref())
            .await
    }

    /// This host's sandboxes that do not record but whose sidecar still
    /// hands its traffic to the recorder: an opt-out stored, then the
    /// control plane stopped before the sidecar was replaced. Only read
    /// when such a sidecar is listed.
    ///
    /// Reads only the rows the handing-off sidecars name (a bounded set:
    /// one per sidecar listed), and none of their policies.
    async fn stray_ids(&self) -> Result<Vec<i32>, LocalTrafficError> {
        let endpoints = self.endpoints().await.map_err(LocalTrafficError::docker(
            WorkloadKind::Sandbox,
            "listing the sandbox egress proxies",
        ))?;
        let mut public_ids: Vec<String> = Vec::new();
        let mut run_ids: Vec<i32> = Vec::new();
        for endpoint in endpoints.iter().filter(|endpoint| endpoint.hands_off) {
            if let Some((ids, run)) = container_sources(&endpoint.sandbox) {
                public_ids.extend(ids);
                run_ids.extend(run);
            }
        }
        if public_ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows = self.stray_candidates(public_ids, run_ids).await.map_err(
            LocalTrafficError::database(WorkloadKind::Sandbox, "reading this host's sandboxes"),
        )?;
        Ok(strays_among(&rows, &endpoints))
    }

    /// This host's live Docker sandboxes that do not record and whose
    /// container may be named after one of `public_ids` or `run_ids`.
    async fn stray_candidates(
        &self,
        public_ids: Vec<String>,
        run_ids: Vec<i32>,
    ) -> Result<Vec<StrayCandidate>, sea_orm::DbErr> {
        let mut named = Condition::any().add(sandboxes::Column::PublicId.is_in(public_ids));
        if !run_ids.is_empty() {
            named = named.add(sandboxes::Column::AgentRunId.is_in(run_ids));
        }
        sandboxes::Entity::find()
            .select_only()
            .columns([
                sandboxes::Column::Id,
                sandboxes::Column::PublicId,
                sandboxes::Column::AgentRunId,
                sandboxes::Column::Status,
            ])
            .filter(sandboxes::Column::EgressRecording.eq(false))
            .filter(sandboxes::Column::NodeId.is_null())
            .filter(sandboxes::Column::Status.ne("destroyed"))
            .filter(
                Condition::any()
                    .add(sandboxes::Column::Backend.is_null())
                    .add(sandboxes::Column::Backend.eq("docker")),
            )
            .filter(named)
            .into_model::<StrayCandidate>()
            .all(self.db.as_ref())
            .await
    }

    /// Every sidecar to bring to the recorder's state when it starts or
    /// stops: the recorded sandboxes', and the strays still handing off.
    /// `None` (and a resync due) when they could not be read.
    async fn sidecars_to_resync(&self) -> Option<Vec<i32>> {
        let mut ids = self.steerable_recorded().await?;
        match self.stray_ids().await {
            Ok(strays) => ids.extend(strays),
            Err(error) => {
                warn!(%error, "egress recording: could not find the sidecars still handing off; they are refreshed once it can");
                self.resync
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                return None;
            }
        }
        ids.sort_unstable();
        ids.dedup();
        Some(ids)
    }

    /// The network policy revision: one row.
    async fn revision(&self) -> Result<i64, sea_orm::DbErr> {
        let row = self
            .db
            .query_one(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT COALESCE((SELECT revision FROM network_policy_state WHERE id = 0), 0)::BIGINT \
                 AS revision",
            ))
            .await?;
        Ok(row
            .map(|row| row.try_get::<i64>("", "revision"))
            .transpose()?
            .unwrap_or_default())
    }

    /// The recorded rows: read again on `discover` or when the revision
    /// moved, otherwise the last read. O(changes): a refresh with nothing
    /// changed costs one one-row query, not every opted-in row.
    ///
    /// A discovery drops the cache before it reads, and a read that fails
    /// leaves none: a policy change the revision does not follow is never
    /// answered from rows read before it, even when the read that should
    /// have followed it failed.
    async fn current_rows(&self, discover: bool) -> Result<Arc<Vec<RecordedRow>>, sea_orm::DbErr> {
        if discover {
            self.forget_rows();
        }
        let read = async {
            let revision = self.revision().await?;
            let cached = self.rows.lock().unwrap_or_else(|e| e.into_inner()).clone();
            if let Some((_, rows)) = cached.filter(|(at, _)| *at == revision) {
                return Ok(rows);
            }
            let rows = Arc::new(self.recorded_rows().await?);
            *self.rows.lock().unwrap_or_else(|e| e.into_inner()) = Some((revision, rows.clone()));
            Ok(rows)
        };
        let rows = read.await;
        if rows.is_err() {
            self.forget_rows();
        }
        rows
    }

    fn forget_rows(&self) {
        *self.rows.lock().unwrap_or_else(|e| e.into_inner()) = None;
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
        if !self.published.load(std::sync::atomic::Ordering::Relaxed) {
            // What a replacement would read is stale: wait for a refresh
            // that publishes, then the retry replaces them.
            for id in ids {
                self.mark_pending(id, true);
            }
            warn!(why, "egress recording: the recorded sandboxes could not be published; their egress proxies are replaced once they are");
            return;
        }
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
    /// replaced now, or `None` when they could not be read: then a resync
    /// is due, and retried on every [`LocalTraffic::maintain`].
    async fn steerable_recorded(&self) -> Option<Vec<i32>> {
        match self.recorded_rows().await {
            Ok(rows) => Some(
                rows.iter()
                    .filter(|row| steerable_status(&row.status))
                    .map(|row| row.id)
                    .collect(),
            ),
            Err(error) => {
                warn!(%error, "egress recording: could not read the recorded sandboxes; their egress proxies are refreshed once it can");
                self.resync
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                None
            }
        }
    }

    /// Refreshes every recorded sidecar, if a start or stop could not.
    async fn retry_resync(&self) {
        if !self
            .resync
            .swap(false, std::sync::atomic::Ordering::Relaxed)
        {
            return;
        }
        if let Some(ids) = self.sidecars_to_resync().await {
            self.refresh_sidecars(ids, "the recorder started or stopped earlier")
                .await;
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
    async fn listen_address(&self) -> Result<Option<IpAddr>, LocalTrafficError> {
        temps_sandbox_providers::docker::egress_network_gateway(&self.docker)
            .await
            .map_err(LocalTrafficError::docker(
                WorkloadKind::Sandbox,
                format!(
                    "inspecting the sandbox egress network '{}'",
                    temps_sandbox_providers::docker::SANDBOX_EGRESS_NETWORK
                ),
            ))
    }

    /// Publishes the recorder's address and the recorded sandboxes for the
    /// sidecars, and returns each one's policy and the addresses of its
    /// sidecar. Reads only: replacing a sidecar is [`Self::maintain`]'s.
    /// Rows are read again on `discover` (an opt-in or a policy changed) or
    /// when the policy revision moved; otherwise the last read is reused.
    async fn refresh(
        &self,
        address: SocketAddr,
        discover: bool,
    ) -> Result<RecordedTraffic, LocalTrafficError> {
        let rows = match self.current_rows(discover).await {
            Ok(rows) => rows,
            Err(source) => {
                self.published
                    .store(false, std::sync::atomic::Ordering::Relaxed);
                return Err(LocalTrafficError::Database {
                    kind: WorkloadKind::Sandbox,
                    operation: "reading the recorded sandboxes",
                    source,
                });
            }
        };
        let names: HashSet<String> = rows.iter().map(RecordedRow::container_name).collect();
        temps_sandbox_providers::docker::publish_egress_recording(Some(
            temps_sandbox_providers::docker::PublishedEgressRecording {
                address,
                installation: self.installation.to_string(),
                sandboxes: names,
            },
        ));
        self.published
            .store(true, std::sync::atomic::Ordering::Relaxed);
        // With nothing recording the sidecars are listed only while one may
        // still hand off (the last list found one, or a restart cannot
        // know) or on a discovery: a quiet host costs no Docker call.
        if rows.is_empty()
            && !discover
            && !self.strays_seen.load(std::sync::atomic::Ordering::Relaxed)
        {
            return Ok(RecordedTraffic::default());
        }
        let endpoints = self.endpoints().await.map_err(LocalTrafficError::docker(
            WorkloadKind::Sandbox,
            "listing the sandbox egress proxies",
        ))?;
        let (traffic, strays) = traffic_from(&rows, &endpoints);
        self.strays_seen
            .store(strays, std::sync::atomic::Ordering::Relaxed);
        Ok(traffic)
    }

    /// Retries the sidecars whose replacement failed, outside the refresh:
    /// replacing one may itself refresh the recorder.
    async fn maintain(&self) {
        self.retry_resync().await;
        self.retry_pending().await;
    }

    /// Once the recorder serves, bring recorded sandboxes' sidecars to its
    /// address. With the port fixed this changes nothing after a restart;
    /// it matters when recording was turned on while the recorder was down.
    /// Recreation happens only on drift, and a failure is retried.
    async fn started(&self, _address: SocketAddr) {
        if let Some(ids) = self.sidecars_to_resync().await {
            self.refresh_sidecars(ids, "the recorder started").await;
        }
    }

    /// The recorder does not serve sandboxes: running sidecars still hand
    /// traffic to its address, so they are replaced without the hand-off
    /// and their sandboxes keep their way out instead of failing every
    /// request until it is back.
    async fn stopped(&self) {
        temps_sandbox_providers::docker::publish_egress_recording(None);
        // No hand-off for anyone is exactly what the sidecars must read now.
        self.published
            .store(true, std::sync::atomic::Ordering::Relaxed);
        // A sidecar a stray opt-out left handing off is released here too:
        // with the recorder gone, its hand-off reaches nothing.
        if let Some(ids) = self.sidecars_to_resync().await {
            self.refresh_sidecars(ids, "the recorder stopped").await;
        }
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

    /// Traffic whose recorded set was published, as after the refresh the
    /// recorder runs before anything is steered.
    fn traffic(db: Arc<DatabaseConnection>, sidecars: Arc<FlakySidecars>) -> SandboxTraffic {
        let traffic = SandboxTraffic::new(
            db,
            bollard::Docker::connect_with_local_defaults().expect("docker client"),
            temps_workload_egress::steering::InstallationId::from_bytes([7; 8]),
            Some(sidecars),
        );
        traffic
            .published
            .store(true, std::sync::atomic::Ordering::Relaxed);
        traffic
    }

    const RECORDER: &str = "127.0.0.1:13128";

    /// A refresh, or `None` (said) when Docker cannot list the sidecars.
    async fn refreshed(traffic: &SandboxTraffic, discover: bool) -> Option<RecordedTraffic> {
        match traffic.refresh(RECORDER.parse().unwrap(), discover).await {
            Ok(traffic) => Some(traffic),
            Err(error @ LocalTrafficError::Docker { .. }) => {
                eprintln!("Docker cannot list containers, skipping: {error}");
                None
            }
            Err(error) => panic!("refresh failed: {error}"),
        }
    }

    /// An opt-in whose refresh could not publish the recorded set does not
    /// replace the sidecar with the stale hand-off (and clear the retry):
    /// it waits, pending, and is replaced once a refresh publishes.
    #[tokio::test]
    async fn steering_waits_for_a_refresh_that_publishes() {
        let Some(f) = fixture().await else { return };
        let conn = f.db.connection();
        conn.execute_unprepared(&format!(
            "UPDATE sandboxes SET egress_recording = true WHERE id = {}",
            ON_CONTROL_PLANE.0
        ))
        .await
        .unwrap();
        let sidecars = FlakySidecars::failing(0);
        let traffic = traffic(f.db.connection_arc(), sidecars.clone());

        conn.execute_unprepared("ALTER TABLE sandboxes RENAME TO sandboxes_unreadable")
            .await
            .unwrap();
        assert!(traffic
            .refresh(RECORDER.parse().unwrap(), true)
            .await
            .is_err());
        conn.execute_unprepared("ALTER TABLE sandboxes_unreadable RENAME TO sandboxes")
            .await
            .unwrap();
        traffic.steer(ON_CONTROL_PLANE.0).await;
        assert!(
            sidecars.applied_to().is_empty(),
            "not with a stale hand-off"
        );
        assert_eq!(
            *traffic.pending.lock().unwrap(),
            HashSet::from([ON_CONTROL_PLANE.0])
        );

        if refreshed(&traffic, true).await.is_none() {
            return;
        }
        traffic.maintain().await;
        assert_eq!(sidecars.applied_to(), vec![ON_CONTROL_PLANE.1.to_string()]);
        assert!(traffic.pending.lock().unwrap().is_empty());
    }

    /// A policy change whose discovery failed to read is still followed: the
    /// next ordinary refresh reads the rows again instead of answering from
    /// the ones read before the change (the revision does not move with a
    /// policy).
    #[tokio::test]
    async fn a_failed_discovery_is_not_answered_from_old_rows() {
        let Some(f) = fixture().await else { return };
        let conn = f.db.connection();
        conn.execute_unprepared(&format!(
            "UPDATE sandboxes SET egress_recording = true WHERE id = {}",
            ON_CONTROL_PLANE.0
        ))
        .await
        .unwrap();
        let traffic = traffic(f.db.connection_arc(), FlakySidecars::failing(0));
        if refreshed(&traffic, false).await.is_none() {
            return;
        }
        conn.execute_unprepared(&format!(
            "UPDATE sandboxes SET egress_policy = jsonb_set(egress_policy::jsonb, '{{mode}}', '\"restricted\"') WHERE id = {}",
            ON_CONTROL_PLANE.0
        ))
        .await
        .unwrap();
        conn.execute_unprepared("ALTER TABLE sandboxes RENAME TO sandboxes_unreadable")
            .await
            .unwrap();
        assert!(traffic
            .refresh(RECORDER.parse().unwrap(), true)
            .await
            .is_err());
        conn.execute_unprepared("ALTER TABLE sandboxes_unreadable RENAME TO sandboxes")
            .await
            .unwrap();
        let after = refreshed(&traffic, false).await.unwrap();
        let subject = WorkloadRef::new(WorkloadKind::Sandbox, i64::from(ON_CONTROL_PLANE.0));
        assert_eq!(
            after.policies[0],
            policy::proxy_policy(
                subject,
                &SandboxEgressPolicy {
                    mode: EgressMode::Restricted,
                    rules: Vec::new(),
                }
            ),
            "the changed policy, not the rows read before it"
        );
    }

    /// With nothing changed a refresh reuses the rows it read; a moved
    /// revision or a discovery (a policy or opt-in change) reads them again.
    #[tokio::test]
    async fn rows_are_read_again_only_when_something_changed() {
        let Some(f) = fixture().await else { return };
        let conn = f.db.connection();
        conn.execute_unprepared(&format!(
            "UPDATE sandboxes SET egress_recording = true WHERE id = {}",
            ON_CONTROL_PLANE.0
        ))
        .await
        .unwrap();
        let traffic = traffic(f.db.connection_arc(), FlakySidecars::failing(0));
        let subject = WorkloadRef::new(WorkloadKind::Sandbox, i64::from(ON_CONTROL_PLANE.0));
        let policy_of = |mode| {
            policy::proxy_policy(
                subject,
                &SandboxEgressPolicy {
                    mode,
                    rules: Vec::new(),
                },
            )
        };
        let set_mode = |mode: &'static str| {
            format!(
                "UPDATE sandboxes SET egress_policy = jsonb_set(egress_policy::jsonb, '{{mode}}', '\"{mode}\"') WHERE id = {}",
                ON_CONTROL_PLANE.0
            )
        };
        let Some(first) = refreshed(&traffic, false).await else {
            return;
        };
        let before = first.policies[0].clone();
        // A policy change moves no revision: the cached rows stay...
        conn.execute_unprepared(&set_mode("restricted"))
            .await
            .unwrap();
        let cached = refreshed(&traffic, false).await.unwrap();
        assert_eq!(cached.policies[0], before, "reused, not read again");
        // ...until it is discovered,
        let discovered = refreshed(&traffic, true).await.unwrap();
        assert_eq!(discovered.policies[0], policy_of(EgressMode::Restricted));
        // or the revision moves.
        conn.execute_unprepared(&set_mode("internet"))
            .await
            .unwrap();
        conn.execute_unprepared(
            "UPDATE network_policy_state SET revision = revision + 1 WHERE id = 0",
        )
        .await
        .unwrap();
        let moved = refreshed(&traffic, false).await.unwrap();
        assert_eq!(moved.policies[0], policy_of(EgressMode::Internet));
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

    /// The recorder started while the recorded sandboxes could not be read:
    /// their sidecars are re-pointed on a later maintenance pass, once the
    /// read works, not left on the old hand-off until the next restart.
    #[tokio::test]
    async fn a_start_whose_read_failed_is_resynced_by_maintenance() {
        let Some(f) = fixture().await else { return };
        let conn = f.db.connection();
        conn.execute_unprepared(&format!(
            "UPDATE sandboxes SET egress_recording = true WHERE id = {}",
            ON_CONTROL_PLANE.0
        ))
        .await
        .unwrap();
        let sidecars = FlakySidecars::failing(0);
        let traffic = traffic(f.db.connection_arc(), sidecars.clone());

        conn.execute_unprepared("ALTER TABLE sandboxes RENAME TO sandboxes_unreadable")
            .await
            .unwrap();
        traffic.started("127.0.0.1:13128".parse().unwrap()).await;
        traffic.maintain().await;
        assert!(sidecars.applied_to().is_empty(), "nothing could be read");

        conn.execute_unprepared("ALTER TABLE sandboxes_unreadable RENAME TO sandboxes")
            .await
            .unwrap();
        traffic.maintain().await;
        assert_eq!(sidecars.applied_to(), vec![ON_CONTROL_PLANE.1.to_string()]);
        traffic.maintain().await;
        assert_eq!(
            sidecars.applied_to().len(),
            1,
            "resynced once, not on every pass"
        );
    }

    /// A changed opt-in replaces the sidecar of a sandbox this host runs,
    /// leaves a worker's alone, and keeps a failure for the next refresh.
    /// A stray is looked up among only the rows its sidecar's name can
    /// belong to: this host's sandboxes that do not record, never another
    /// node's, and a name no row has finds nothing.
    #[tokio::test]
    async fn stray_candidates_are_only_the_named_rows_of_this_host() {
        let Some(f) = fixture().await else { return };
        let traffic = traffic(f.db.connection_arc(), FlakySidecars::failing(0));
        let found = traffic
            .stray_candidates(
                vec![
                    ON_CONTROL_PLANE.1.to_string(),
                    ON_WORKER.1.to_string(),
                    "sbx_ffffffffffffffff".to_string(),
                ],
                vec![],
            )
            .await
            .unwrap();
        let ids: Vec<i32> = found.iter().map(|row| row.id).collect();
        assert_eq!(ids, [ON_CONTROL_PLANE.0], "{found:?}");
        assert!(traffic
            .stray_candidates(vec!["sbx_ffffffffffffffff".to_string()], vec![987_654])
            .await
            .unwrap()
            .is_empty());
    }

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

    /// Only sidecars handing off to this installation's recorder count: a
    /// sidecar another installation on the same daemon started (the same
    /// container name, a matching row here) is never attributed to this
    /// installation's sandbox, and neither is one that does not hand off or
    /// whose label names nobody.
    #[test]
    fn only_sidecars_handing_off_to_this_installation_count() {
        let ours = InstallationId::from_bytes([7; 8]);
        let endpoint =
            |address: &str, hands_off: bool, installation: Option<&str>| EgressProxyEndpoint {
                sandbox: "temps-sandbox-aaaa".into(),
                address: address.parse().unwrap(),
                hands_off,
                installation: installation.map(str::to_string),
            };
        let kept = handing_off_here(
            vec![
                endpoint("172.30.0.2", true, Some("0707070707070707")),
                endpoint("172.30.0.3", true, Some("0909090909090909")),
                endpoint("172.30.0.4", true, None),
                endpoint("172.30.0.5", false, None),
            ],
            ours,
        );
        let addresses: Vec<String> = kept.iter().map(|e| e.address.to_string()).collect();
        assert_eq!(addresses, ["172.30.0.2"]);

        let row = RecordedRow {
            id: 7,
            public_id: "sbx_aaaa".into(),
            agent_run_id: None,
            status: "running".into(),
            egress_policy: Default::default(),
        };
        let foreign = handing_off_here(
            vec![endpoint("172.30.0.3", true, Some("0909090909090909"))],
            ours,
        );
        let (traffic, strays) = traffic_from(&[row], &foreign);
        assert!(!strays);
        assert_eq!(
            traffic.identities.address_count(),
            0,
            "another installation's sidecar maps to nothing here"
        );
    }

    /// A stray is looked up by what its sidecar's name could have been
    /// built from, and every row a name can come from is among them.
    #[test]
    fn a_sidecar_name_leads_back_to_the_rows_it_can_belong_to() {
        assert_eq!(
            container_sources("temps-sandbox-aaaa"),
            Some((vec!["sbx_aaaa".to_string(), "aaaa".to_string()], None))
        );
        assert_eq!(
            container_sources("temps-sandbox-42"),
            Some((vec!["sbx_42".to_string(), "42".to_string()], Some(42)))
        );
        assert_eq!(container_sources("temps-sandbox-"), None);
        assert_eq!(container_sources("something-else"), None);
        for (public_id, run) in [("sbx_aaaa", None), ("sbx_bbbb", Some(42))] {
            let name = container_of(public_id, run);
            let (ids, run_id) = container_sources(&name).unwrap();
            assert!(
                ids.iter().any(|id| id == public_id) || run_id == run,
                "{name} leads back to {public_id}/{run:?}"
            );
        }
    }

    /// Sidecars are found by name in one pass, whatever the number of
    /// recorded sandboxes.
    #[test]
    fn endpoints_are_indexed_by_sandbox() {
        let endpoint = |sandbox: &str, address: &str| EgressProxyEndpoint {
            sandbox: sandbox.into(),
            address: address.parse().unwrap(),
            hands_off: true,
            installation: None,
        };
        let endpoints = [
            endpoint("temps-sandbox-a", "172.30.0.2"),
            endpoint("temps-sandbox-b", "172.30.0.3"),
            endpoint("temps-sandbox-a", "172.30.0.4"),
        ];
        let index = endpoints_by_sandbox(&endpoints);
        let address = |a: &str| a.parse::<IpAddr>().unwrap();
        assert_eq!(
            index["temps-sandbox-a"],
            vec![address("172.30.0.2"), address("172.30.0.4")]
        );
        assert_eq!(index["temps-sandbox-b"], vec![address("172.30.0.3")]);
        assert!(!index.contains_key("temps-sandbox-c"));
    }

    /// The sandboxes a recorder start or stop must also release: those that
    /// do not record but whose sidecar still hands off, and can be replaced
    /// now. One that does not hand off, or is not running or stopped, is
    /// left alone.
    #[test]
    fn strays_are_the_non_recording_sandboxes_whose_sidecar_hands_off() {
        let row = |id: i32, public_id: &str, status: &str| StrayCandidate {
            id,
            public_id: public_id.into(),
            agent_run_id: None,
            status: status.into(),
        };
        let endpoint = |sandbox: &str, hands_off: bool| EgressProxyEndpoint {
            sandbox: sandbox.into(),
            address: "172.30.0.9".parse().unwrap(),
            hands_off,
            installation: None,
        };
        let rows = [
            row(1, "sbx_aaaa", "running"),
            row(2, "sbx_bbbb", "running"),
            row(3, "sbx_cccc", "starting"),
            row(4, "sbx_dddd", "stopped"),
        ];
        let endpoints = [
            endpoint("temps-sandbox-aaaa", true),
            endpoint("temps-sandbox-bbbb", false),
            endpoint("temps-sandbox-cccc", true),
            endpoint("temps-sandbox-dddd", true),
        ];
        assert_eq!(strays_among(&rows, &endpoints), vec![1, 4]);
    }

    /// A sidecar still handing off for a sandbox that no longer records
    /// (its opt-out was stored, then the control plane stopped before the
    /// sidecar was replaced) gets through unrecorded instead of refused as
    /// unknown. One that does not hand off is not in the table at all.
    #[test]
    fn a_stray_hand_off_is_let_through_unrecorded() {
        let recorded = RecordedRow {
            id: 7,
            public_id: "sbx_aaaa".into(),
            agent_run_id: None,
            status: "running".into(),
            egress_policy: Default::default(),
        };
        let endpoints = [
            EgressProxyEndpoint {
                sandbox: "temps-sandbox-aaaa".into(),
                address: "172.30.0.2".parse().unwrap(),
                hands_off: true,
                installation: None,
            },
            EgressProxyEndpoint {
                sandbox: "temps-sandbox-bbbb".into(),
                address: "172.30.0.3".parse().unwrap(),
                hands_off: true,
                installation: None,
            },
            EgressProxyEndpoint {
                sandbox: "temps-sandbox-cccc".into(),
                address: "172.30.0.4".parse().unwrap(),
                hands_off: false,
                installation: None,
            },
        ];
        let (traffic, strays) = traffic_from(&[recorded], &endpoints);
        assert!(strays);
        let table =
            temps_workload_netpol::EgressTable::build(&traffic.policies, &traffic.identities);
        let at = |a: &str| {
            table
                .lookup(a.parse().unwrap())
                .map(|rules| (rules.workload, rules.record))
        };
        assert_eq!(
            at("172.30.0.2"),
            Some((WorkloadRef::new(WorkloadKind::Sandbox, 7), true))
        );
        assert_eq!(
            at("172.30.0.3"),
            Some((STRAY, false)),
            "through, never recorded"
        );
        assert_eq!(at("172.30.0.4"), None);
        let (_, none) = traffic_from(&[], &endpoints[2..]);
        assert!(!none, "nothing hands off: nothing to keep listing for");
    }

    #[test]
    fn container_names_follow_the_sandbox_public_id() {
        assert_eq!(
            container_name("sbx_10a5f86bd8ff859d"),
            "temps-sandbox-10a5f86bd8ff859d"
        );
    }

    /// A sandbox an agent run started is named after the run, as the
    /// Docker provider names it, not after its random public id: a name
    /// built from the public id would never match its sidecar.
    #[test]
    fn an_agent_run_sandbox_is_named_after_its_run() {
        let row = |agent_run_id| sandboxes::Model {
            public_id: "sbx_10a5f86bd8ff859d".to_string(),
            agent_run_id,
            ..sandbox_row()
        };
        assert_eq!(row_container_name(&row(Some(42))), "temps-sandbox-42");
        assert_eq!(
            row_container_name(&row(None)),
            "temps-sandbox-10a5f86bd8ff859d"
        );
        assert_eq!(
            temps_sandbox_providers::docker::SANDBOX_NAME_PREFIX,
            SANDBOX_CONTAINER_PREFIX,
            "the provider's prefix"
        );
    }

    fn sandbox_row() -> sandboxes::Model {
        let now = chrono::Utc::now();
        sandboxes::Model {
            id: 1,
            public_id: String::new(),
            user_id: None,
            agent_run_id: None,
            name: String::new(),
            status: "running".into(),
            lifecycle: String::new(),
            project_id: None,
            source_repo_url: None,
            image: None,
            work_dir: String::new(),
            timeout_secs: 0,
            metadata: None,
            backend: None,
            node_id: None,
            egress_policy: Default::default(),
            egress_recording: true,
            created_at: now,
            last_activity_at: now,
            expires_at: now,
            preview_password_hash: None,
            preview_password_hint: None,
            preview_access: Default::default(),
            preview_allowed_cidrs: Vec::new(),
        }
    }
}
