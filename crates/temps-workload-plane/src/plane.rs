// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The plane's API: apply, wait, read, delete.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sea_orm::sea_query::Expr;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseBackend, DatabaseConnection, EntityTrait, PaginatorTrait,
    QueryFilter, QueryOrder, QuerySelect, Statement,
};
use temps_core::AuditLogger;
use temps_entities::{workload_instances, workloads};
use temps_workload::{
    ConditionKind, ConditionStatus, Isolation, RuntimeDriver, WorkloadPhase, WorkloadSpec,
    WorkloadStatus,
};
use temps_workload_env::Redactor;
use tokio::sync::{watch, Notify};
use tokio::task::JoinHandle;

use crate::admission::{admit, AdmissionInput, AdmissionPolicy};
use crate::audit::AdmissionRefusedAudit;
use crate::env::EnvSource;
use crate::error::PlaneError;
use crate::events;
use crate::inventory::{InFlightGuard, InFlightLedger, NodeInventory, PlacementHints};
use crate::node::Placement as AttemptPlacement;
use crate::scheduler::{
    self, Explanation, Placement, ScheduleRequest, SchedulerConfig, UnifiedScheduler,
};
use crate::stream::StreamMetrics;
use crate::volumes::{bind_source, pin_for, VolumeStore};
use temps_workload_volumes::{VolumePin, VolumeProvider};

/// The product object that owns a workload: `cron/7`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Owner {
    pub kind: String,
    pub id: i64,
}

impl Owner {
    pub fn new(kind: impl Into<String>, id: i64) -> Self {
        Self {
            kind: kind.into(),
            id,
        }
    }
}

impl fmt::Display for Owner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.kind, self.id)
    }
}

/// One `apply`.
#[derive(Debug, Clone)]
pub struct ApplyRequest {
    pub owner: Owner,
    /// Unique per owner among live workloads; applying the same name again
    /// with the same spec is a no-op.
    pub name: String,
    pub spec: WorkloadSpec,
    /// The project the workload runs for, for the ADR-045 grant.
    pub project_slug: Option<String>,
    /// Finalizers the workload is created with. Each must be registered on
    /// the plane ([`WorkloadPlane::with_finalizer`]) or deletion waits.
    pub finalizers: Vec<String>,
}

/// How the plane is configured. Built once at startup.
#[derive(Debug, Clone)]
pub struct PlaneConfig {
    /// Value of the `sh.temps.plane` label on every instance this plane
    /// creates, and the prefix of their names. Orphan cleanup only touches
    /// instances with this value.
    pub scope: String,
    /// The network instances boot on (`temps_core::NETWORK_NAME`).
    pub network: Option<String>,
    pub admission: AdmissionPolicy,
    /// The slow resync: a full pass (orphan cleanup, event pruning) at
    /// least this often.
    pub resync_interval: Duration,
    /// How often a running instance is inspected when the driver cannot
    /// report exits, and how soon a failed pass is retried.
    pub poll_interval: Duration,
    /// Lines of output kept for a finished run.
    pub output_tail_lines: u32,
    /// Host interface a long-running instance's ports are published on
    /// (the runtime picks the port). Never `0.0.0.0`: the proxy reaches the
    /// instance over loopback or the node's private address.
    pub publish_host_ip: String,
    /// Most workloads one pass reads ([`crate::reconciler::PASS_LIMIT`]).
    /// A pass that fills it resumes after the last id it read, so rows past
    /// the first batch are always reached.
    pub pass_limit: u64,
    /// Resolvers for instances this plane creates on the control plane:
    /// the proxy's cluster DNS resolver (ADR-024) when cluster DNS is
    /// enabled, so `*.temps.local` names resolve as they do for legacy
    /// deployments. Worker nodes use their own resolver (the node
    /// runtime's), never this one.
    pub cluster_dns: Option<temps_workload::ResolverSource>,
}

impl PlaneConfig {
    pub fn new(scope: impl Into<String>) -> Self {
        Self {
            scope: scope.into(),
            network: None,
            admission: AdmissionPolicy::default(),
            resync_interval: Duration::from_secs(300),
            poll_interval: Duration::from_secs(5),
            output_tail_lines: 50,
            publish_host_ip: "127.0.0.1".to_string(),
            pass_limit: crate::reconciler::PASS_LIMIT,
            cluster_dns: None,
        }
    }
}

/// The runtime driver of the control plane (node 0), registered by `temps
/// serve`. `None` when this host has no runtime: workloads then fail with
/// `NoEligibleNode` instead of waiting.
#[derive(Clone, Default)]
pub struct NodeDriver(pub Option<Arc<dyn RuntimeDriver>>);

impl fmt::Debug for NodeDriver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            Some(driver) => write!(f, "NodeDriver({})", driver.capabilities().name),
            None => f.write_str("NodeDriver(none)"),
        }
    }
}

/// A workload as the plane reports it.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkloadView {
    pub id: i64,
    pub owner: Owner,
    pub name: String,
    pub spec: WorkloadSpec,
    pub status: WorkloadStatus,
    pub generation: i64,
    pub observed_generation: i64,
    pub change_seq: i64,
    pub finalizers: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
    /// The latest attempt (instance) number; 0 before the first started.
    pub attempt: u32,
    /// When the latest attempt finished.
    pub finished_at: Option<DateTime<Utc>>,
    /// The last lines of the latest attempt's output, with secret values
    /// redacted. Only filled for finalizers; `None` when unavailable.
    pub output_tail: Option<String>,
}

/// Cleanup a product runs before a workload of its is removed (a run
/// history row, a role drop). The plane calls it once the workload is
/// finished or deleted, and removes the finalizer when it returns `Ok`;
/// on `Err` it retries on a later pass. Must be idempotent.
#[async_trait]
pub trait Finalizer: Send + Sync {
    fn name(&self) -> &str;
    async fn finalize(&self, workload: &WorkloadView) -> Result<(), String>;
}

/// The running reconciler. Dropping it leaves the loop running;
/// [`ReconcilerHandle::abort`] stops it and every exit watcher at once,
/// which is what a crash looks like to the next plane.
pub struct ReconcilerHandle {
    pub(crate) task: JoinHandle<()>,
    pub(crate) plane: Arc<WorkloadPlane>,
}

impl ReconcilerHandle {
    pub fn abort(self) {
        self.task.abort();
        self.plane.abort_watchers();
    }
}

pub struct WorkloadPlane {
    pub(crate) db: Arc<DatabaseConnection>,
    pub(crate) driver: NodeDriver,
    pub(crate) env: Arc<dyn EnvSource>,
    pub(crate) audit: Arc<dyn AuditLogger>,
    pub(crate) config: PlaneConfig,
    pub(crate) finalizers: BTreeMap<String, Arc<dyn Finalizer>>,
    /// Wakes the reconciler: on apply, on delete, on an instance exit.
    pub(crate) notify: Arc<Notify>,
    /// Bumped on every status write, for `apply_and_wait`.
    pub(crate) status_tx: watch::Sender<u64>,
    pub(crate) watchers: Mutex<HashMap<i64, JoinHandle<()>>>,
    /// Redactors of runs started by this process, by workload id.
    pub(crate) redactors: Mutex<HashMap<i64, Redactor>>,
    /// Cleared when the driver reports it cannot wait for exits.
    pub(crate) driver_waits: Arc<AtomicBool>,
    pub(crate) started: AtomicBool,
    /// Where the scheduler's node facts come from; `None` = control plane
    /// only (phase 2).
    pub(crate) inventory: Option<Arc<dyn NodeInventory>>,
    pub(crate) scheduler: Arc<UnifiedScheduler>,
    pub(crate) in_flight: Arc<InFlightLedger>,
    /// Serialises scheduling: reading the node facts (with the in-flight
    /// ledger applied), deciding, and holding the decision's reservation
    /// are one critical section, so two concurrent placements cannot both
    /// admit into the same headroom. An async lock: the facts are read
    /// across an await. Placements are control-plane work, a handful at a
    /// time; this never sits on a request path.
    pub(crate) scheduling: tokio::sync::Mutex<()>,
    /// Environments staged for one create, by import name (`staged:<key>`):
    /// never stored in a spec.
    pub(crate) staged_env: StagedEnvs,
    /// Where new attempts run. `None`: on the control plane (node 0).
    pub(crate) placement: Option<Arc<dyn AttemptPlacement>>,
    pub(crate) stream_metrics: StreamMetrics,
    /// Creates volume backings on the control plane (node 0). `None`: the
    /// runtime creates a named volume on first bind, without labels.
    pub(crate) volume_provider: Option<Arc<dyn VolumeProvider>>,
    /// More runtimes on the control plane next to `driver`: the WASM
    /// driver for functions, a Firecracker driver for `micro_vm`
    /// deployments. Empty on most hosts. See
    /// [`WorkloadPlane::with_local_driver`] and
    /// [`WorkloadPlane::with_control_plane_driver`].
    pub(crate) local_drivers: Vec<Arc<dyn RuntimeDriver>>,
    /// The last workload id the previous pass read when it filled
    /// [`PlaneConfig::pass_limit`]; `0` starts from the lowest id.
    pub(crate) pass_cursor: AtomicI64,
}

/// Retires a workload when dropped, unless disarmed. A caller that applies a
/// workload and then waits on it (a deployment handing a replica over) holds
/// one, so a wait that fails, times out or is cancelled never leaves a
/// workload, and its container, that no caller knows about.
#[must_use = "dropping the guard retires the workload at once"]
pub struct RetireOnDrop {
    plane: Arc<WorkloadPlane>,
    workload_id: i64,
    reason: &'static str,
    message: String,
    armed: bool,
}

impl RetireOnDrop {
    pub fn workload_id(&self) -> i64 {
        self.workload_id
    }

    /// The handoff succeeded: keep the workload.
    pub fn disarm(mut self) {
        self.armed = false;
    }

    /// Retire the workload now, with `message` as why.
    pub async fn retire(mut self, message: &str) -> Result<(), PlaneError> {
        self.armed = false;
        self.plane
            .delete(self.workload_id, self.reason, message)
            .await
    }
}

impl fmt::Debug for RetireOnDrop {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RetireOnDrop")
            .field("workload_id", &self.workload_id)
            .field("armed", &self.armed)
            .finish()
    }
}

impl Drop for RetireOnDrop {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let plane = self.plane.clone();
        let workload_id = self.workload_id;
        let reason = self.reason;
        let message = std::mem::take(&mut self.message);
        match tokio::runtime::Handle::try_current() {
            Ok(runtime) => {
                runtime.spawn(async move {
                    if let Err(error) = plane.delete(workload_id, reason, &message).await {
                        tracing::error!(
                            workload_id,
                            "retiring abandoned workload {workload_id} failed: {error}"
                        );
                    }
                });
            }
            Err(_) => tracing::error!(
                workload_id,
                "workload {workload_id} was abandoned outside a runtime and could not be \
                 retired; the next delete of its owner removes it"
            ),
        }
    }
}

/// The plane, bound once it starts. Plugins that register before
/// `temps serve` starts the plane hold the slot and read it per call; an
/// empty slot means this process runs no plane.
#[derive(Debug, Default)]
pub struct PlaneSlot(std::sync::OnceLock<Arc<WorkloadPlane>>);

impl PlaneSlot {
    pub fn new() -> Self {
        Self::default()
    }

    /// Bind the plane. `false` when one was already bound (it is kept).
    pub fn set(&self, plane: Arc<WorkloadPlane>) -> bool {
        self.0.set(plane).is_ok()
    }

    pub fn get(&self) -> Option<Arc<WorkloadPlane>> {
        self.0.get().cloned()
    }
}

/// What [`WorkloadPlane::apply_with_volumes`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeApplied {
    pub workload_id: i64,
    /// Where the volumes pin the workload, and why (`None` without
    /// volumes). Its `Display` is the console's "pinned to node 0 by volume
    /// `data`".
    pub pin: Option<VolumePin>,
}

/// Environments staged in this process, by key.
pub(crate) type StagedEnvs = Arc<Mutex<HashMap<String, crate::env::StagedEnv>>>;

/// The import name of an environment staged with
/// [`WorkloadPlane::stage_env`].
pub const STAGED_IMPORT_PREFIX: &str = "staged:";

/// Removes a staged environment when dropped.
#[derive(Debug)]
pub struct StagedEnvGuard {
    staged: StagedEnvs,
    key: String,
}

impl StagedEnvGuard {
    /// The import to put in `EnvSpec::imports`.
    pub fn import(&self) -> String {
        format!("{STAGED_IMPORT_PREFIX}{}", self.key)
    }
}

impl Drop for StagedEnvGuard {
    fn drop(&mut self) {
        self.staged
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&self.key);
    }
}

impl fmt::Debug for WorkloadPlane {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WorkloadPlane")
            .field("scope", &self.config.scope)
            .field("driver", &self.driver)
            .field("finalizers", &self.finalizers.keys().collect::<Vec<_>>())
            .field("placement", &self.placement.is_some())
            .field(
                "local_drivers",
                &self
                    .local_drivers
                    .iter()
                    .map(|driver| driver.capabilities().name.clone())
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl WorkloadPlane {
    pub fn new(
        db: Arc<DatabaseConnection>,
        driver: NodeDriver,
        env: Arc<dyn EnvSource>,
        audit: Arc<dyn AuditLogger>,
        config: PlaneConfig,
    ) -> Self {
        Self {
            db,
            driver,
            env,
            audit,
            config,
            finalizers: BTreeMap::new(),
            notify: Arc::new(Notify::new()),
            status_tx: watch::channel(0).0,
            watchers: Mutex::new(HashMap::new()),
            redactors: Mutex::new(HashMap::new()),
            driver_waits: Arc::new(AtomicBool::new(true)),
            started: AtomicBool::new(false),
            inventory: None,
            scheduler: Arc::new(UnifiedScheduler::new(SchedulerConfig::new(
                scheduler::platforms_equal,
            ))),
            in_flight: InFlightLedger::new(),
            scheduling: tokio::sync::Mutex::new(()),
            staged_env: Arc::new(Mutex::new(HashMap::new())),
            placement: None,
            stream_metrics: StreamMetrics::default(),
            volume_provider: None,
            local_drivers: Vec::new(),
            pass_cursor: AtomicI64::new(0),
        }
    }

    /// Create volume backings on the control plane through `provider`.
    pub fn with_volume_provider(mut self, provider: Arc<dyn VolumeProvider>) -> Self {
        self.volume_provider = Some(provider);
        self
    }

    /// Add a runtime on the control plane next to the node driver (the
    /// WASM driver). The plane picks it by capability for a scale-to-zero
    /// workload whose artifact, isolation or egress the node driver cannot
    /// serve; jobs and `process` deployment replicas keep the node driver.
    pub fn with_local_driver(mut self, driver: Arc<dyn RuntimeDriver>) -> Self {
        self.local_drivers.push(driver);
        self
    }

    /// Place new attempts through `placement` (workers included) instead
    /// of always on the control plane.
    pub fn with_placement(mut self, placement: Arc<dyn AttemptPlacement>) -> Self {
        self.placement = Some(placement);
        self
    }

    /// Feed the scheduler from `inventory` (ADR-055 §8).
    pub fn with_inventory(mut self, inventory: Arc<dyn NodeInventory>) -> Self {
        self.inventory = Some(inventory);
        self
    }

    pub fn with_scheduler_config(mut self, config: SchedulerConfig) -> Self {
        self.scheduler = Arc::new(UnifiedScheduler::new(config));
        self
    }

    pub fn scheduler(&self) -> &UnifiedScheduler {
        &self.scheduler
    }

    /// Also run node-0 workloads that request `driver`'s isolation through
    /// it (ADR-055 §6: "Firecracker for deployments needs no new driver,
    /// only a different isolation request"). A driver whose isolation the
    /// primary driver or an earlier one already provides is ignored, so the
    /// choice per isolation is fixed at startup.
    pub fn with_control_plane_driver(mut self, driver: Arc<dyn RuntimeDriver>) -> Self {
        let isolation = driver.capabilities().isolation;
        if self.control_plane_isolations().contains(&isolation) {
            tracing::warn!(
                driver = %driver.capabilities().name,
                isolation = ?isolation,
                "a node-0 driver for this isolation is already registered; ignoring the second"
            );
            return self;
        }
        self.local_drivers.push(driver);
        self
    }

    /// The isolations the control plane (node 0) can run: its primary
    /// driver's (process, also when this host has no daemon, so the reason a
    /// process workload fails stays `NoEligibleNode` at the reconciler) and
    /// every local driver ([`Self::with_control_plane_driver`],
    /// [`Self::with_local_driver`]).
    pub fn control_plane_isolations(&self) -> Vec<Isolation> {
        let primary = self
            .driver
            .0
            .as_ref()
            .map(|driver| driver.capabilities().isolation)
            .unwrap_or(Isolation::Process);
        let mut isolations = vec![primary];
        for driver in &self.local_drivers {
            let isolation = driver.capabilities().isolation;
            if !isolations.contains(&isolation) {
                isolations.push(isolation);
            }
        }
        isolations
    }

    /// The node-0 driver that provides `isolation`, or why there is none.
    pub(crate) fn control_plane_driver_for(
        &self,
        isolation: Isolation,
    ) -> Result<Arc<dyn RuntimeDriver>, String> {
        if let Some(driver) = self.driver.0.as_ref() {
            if driver.capabilities().isolation == isolation {
                return Ok(driver.clone());
            }
        }
        if let Some(driver) = self
            .local_drivers
            .iter()
            .find(|driver| driver.capabilities().isolation == isolation)
        {
            return Ok(driver.clone());
        }
        match (&self.driver.0, isolation) {
            (None, Isolation::Process) => Err(NO_CONTROL_PLANE_DRIVER.to_string()),
            _ => Err(no_driver_for_isolation(
                isolation,
                &self.control_plane_driver_names(),
            )),
        }
    }

    /// Every node-0 driver, the primary first.
    pub(crate) fn control_plane_drivers(&self) -> Vec<Arc<dyn RuntimeDriver>> {
        self.driver
            .0
            .iter()
            .chain(self.local_drivers.iter())
            .cloned()
            .collect()
    }

    fn control_plane_driver_names(&self) -> Vec<String> {
        self.control_plane_drivers()
            .iter()
            .map(|driver| driver.capabilities().name.clone())
            .collect()
    }

    /// Stage `vars` for one create: the spec imports
    /// [`StagedEnvGuard::import`] instead of holding the values, so a
    /// deployment's variables (secrets among them) are never written into
    /// the stored spec. The values live in this process's memory until the
    /// guard drops; an instance recreated after a restart cannot read them
    /// and fails with `EnvironmentUnavailable`.
    pub fn stage_env(&self, key: &str, vars: Vec<(String, String)>) -> StagedEnvGuard {
        self.stage_env_with_files(key, vars, Vec::new())
    }

    /// [`Self::stage_env`], with secret files delivered at
    /// `/run/secrets/<name>` (the deployment's ADR-019 secrets). Their
    /// values are held the same way: in memory only, never in a spec.
    pub fn stage_env_with_files(
        &self,
        key: &str,
        vars: Vec<(String, String)>,
        secret_files: Vec<temps_workload::SecretFile>,
    ) -> StagedEnvGuard {
        self.staged_env
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(
                key.to_string(),
                crate::env::StagedEnv { vars, secret_files },
            );
        StagedEnvGuard {
            staged: Arc::clone(&self.staged_env),
            key: key.to_string(),
        }
    }

    pub(crate) fn staged(&self, import: &str) -> Option<crate::env::StagedEnv> {
        let key = import.strip_prefix(STAGED_IMPORT_PREFIX)?;
        self.staged_env
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(key)
            .cloned()
    }

    async fn facts_for(
        &self,
        workload: &str,
        hints: &PlacementHints,
    ) -> Result<Vec<scheduler::NodeFacts>, PlaneError> {
        let inventory = self
            .inventory
            .as_ref()
            .ok_or_else(|| PlaneError::NoInventory {
                scope: self.config.scope.clone(),
                workload: workload.to_string(),
            })?;
        let mut nodes = inventory
            .nodes(hints.project_slug.as_deref())
            .await
            .map_err(|reason| PlaneError::Inventory {
                workload: workload.to_string(),
                reason,
            })?;
        self.in_flight.apply(&mut nodes);
        // The inventory knows rows, not this process's drivers: node 0
        // provides what its registered drivers provide.
        let isolations = self.control_plane_isolations();
        for node in nodes
            .iter_mut()
            .filter(|node| node.id == scheduler::CONTROL_PLANE_NODE_ID)
        {
            node.isolations = isolations.clone();
        }
        Ok(nodes)
    }

    fn request_for<'a>(
        &self,
        spec: &'a WorkloadSpec,
        hints: &'a PlacementHints,
    ) -> ScheduleRequest<'a> {
        ScheduleRequest {
            kind: spec.owner.kind,
            placement: &spec.placement,
            resources: &spec.resources,
            isolation: spec.isolation,
            image_platforms: &hints.image_platforms,
            socket_gate: self
                .inventory
                .as_ref()
                .and_then(|inventory| inventory.socket_gate(hints.project_slug.as_deref())),
            exclude_node_ids: &hints.exclude_node_ids,
            exclude_control_plane: hints.exclude_control_plane,
            affinity_nodes: &hints.affinity_nodes,
        }
    }

    /// Place `spec` (strictly: a pin that cannot be met is an error, never
    /// a quiet move). The reservation of each replica is held as in flight
    /// until the returned guard drops, so concurrent placements see it.
    pub async fn schedule(
        &self,
        name: &str,
        spec: &WorkloadSpec,
        hints: &PlacementHints,
    ) -> Result<(Placement, InFlightGuard), PlaneError> {
        // Held until the reservation is in the ledger: another placement
        // reads the facts only after this one's hold is counted in them.
        let _scheduling = self.scheduling.lock().await;
        let nodes = self.facts_for(name, hints).await?;
        let placement = self
            .scheduler
            .schedule(&nodes, &self.request_for(spec, hints))
            .map_err(|error| PlaneError::Unschedulable {
                workload: name.to_string(),
                reason: error.reason(),
                message: error.to_string(),
            })?;
        let guard = self.in_flight.hold(&placement.nodes, placement.reservation);
        Ok((placement, guard))
    }

    /// A scheduler dry run for `spec` (ADR-055 §8): the recommended nodes
    /// with reasons (linked service's node, image cached, architecture,
    /// headroom), the nodes passed over and why, and where it would go now.
    /// Changes nothing.
    pub async fn explain(
        &self,
        spec: &WorkloadSpec,
        hints: &PlacementHints,
    ) -> Result<Explanation, PlaneError> {
        let name = format!("{}", spec.owner);
        let nodes = self.facts_for(&name, hints).await?;
        let image = match &spec.artifact {
            temps_workload::Artifact::OciImage { reference, .. } => Some(reference.as_str()),
            _ => None,
        };
        let context = match self.inventory.as_ref() {
            Some(inventory) => inventory
                .explain_context(hints, image)
                .await
                .map_err(|reason| PlaneError::Inventory {
                    workload: name.clone(),
                    reason,
                })?,
            None => Default::default(),
        };
        Ok(scheduler::explain(
            &self.scheduler,
            &nodes,
            &self.request_for(spec, hints),
            &context,
        ))
    }

    /// The latest instance of workload `id` and the driver that runs it.
    /// `None` before the first instance exists, or when no control-plane
    /// driver serves it (this host has no runtime). A worker's instance
    /// whose node cannot be reached is an error, not `None`.
    async fn latest_instance(
        &self,
        id: i64,
    ) -> Result<Option<(temps_workload::InstanceHandle, Arc<dyn RuntimeDriver>)>, PlaneError> {
        let latest = workload_instances::Entity::find()
            .filter(workload_instances::Column::WorkloadId.eq(id))
            .order_by_desc(workload_instances::Column::Attempt)
            .limit(1)
            .one(self.db.as_ref())
            .await
            .map_err(PlaneError::db("reading the latest instance", id))?;
        let Some(latest) = latest else {
            return Ok(None);
        };
        let handle: temps_workload::InstanceHandle = serde_json::from_value(latest.handle.clone())
            .map_err(|e| PlaneError::Corrupt {
                workload_id: id,
                field: "instance handle",
                reason: format!("instance {}: {e}", latest.id),
            })?;
        // The driver of the node the instance is on: on the control plane
        // the runtime that created it (the node driver, or a local one such
        // as WASM or Firecracker); a worker's replica is answered from its
        // node's reports.
        match self.driver_of(&latest).await {
            Ok(driver) => Ok(Some((handle, driver))),
            Err(reason) => match latest.node_id {
                None => {
                    tracing::debug!(workload_id = id, "no driver serves the instance: {reason}");
                    Ok(None)
                }
                Some(node_id) => Err(PlaneError::NodeUnavailable {
                    workload_id: id,
                    node_id,
                    reason,
                }),
            },
        }
    }

    /// The latest instance of workload `id` and what the driver reports for
    /// it now (state, address, published ports). `None` before the first
    /// instance exists or without a driver.
    pub async fn instance_status(
        &self,
        id: i64,
    ) -> Result<
        Option<(
            temps_workload::InstanceHandle,
            temps_workload::InstanceStatus,
        )>,
        PlaneError,
    > {
        let Some((handle, driver)) = self.latest_instance(id).await? else {
            return Ok(None);
        };
        let status = driver
            .inspect(&handle)
            .await
            .map_err(|source| PlaneError::Driver {
                workload_id: id,
                operation: "inspect the instance",
                source,
            })?;
        Ok(Some((handle, status)))
    }

    /// What workload `id`'s latest instance is using right now, from its
    /// driver (`None` when it has no instance yet). For products whose
    /// instance is not a container the host's Docker can read (a microVM).
    pub async fn instance_stats(
        &self,
        id: i64,
    ) -> Result<Option<temps_workload::InstanceStats>, PlaneError> {
        let Some((handle, driver)) = self.latest_instance(id).await? else {
            return Ok(None);
        };
        driver
            .stats(&handle)
            .await
            .map(Some)
            .map_err(|source| PlaneError::Driver {
                workload_id: id,
                operation: "read the instance's stats",
                source,
            })
    }

    /// The last `tail` output lines of workload `id`'s latest instance, from
    /// its driver (`None` when it has no instance yet). For products whose
    /// instance is not a container the host's Docker can read (a microVM).
    pub async fn instance_logs(
        &self,
        id: i64,
        tail: u32,
    ) -> Result<Option<Vec<temps_workload::LogLine>>, PlaneError> {
        use futures::TryStreamExt;
        let Some((handle, driver)) = self.latest_instance(id).await? else {
            return Ok(None);
        };
        let driver_error = |source| PlaneError::Driver {
            workload_id: id,
            operation: "read the instance's logs",
            source,
        };
        let stream = driver
            .logs(
                &handle,
                temps_workload::LogQuery {
                    since: None,
                    tail: Some(tail),
                    follow: false,
                    resume: false,
                },
            )
            .await
            .map_err(driver_error)?;
        let lines: Vec<temps_workload::LogLine> =
            stream.try_collect().await.map_err(driver_error)?;
        Ok(Some(lines))
    }

    /// Follow workload `id`'s latest instance's output for ingestion: from
    /// the checkpoint its driver keeps for the previous follower (the start
    /// of what is retained on the first run), continuing as lines arrive.
    /// `None` when it has no instance yet. The stream ends with an error when
    /// the instance cannot be reached any more.
    pub async fn follow_instance_logs(
        &self,
        id: i64,
    ) -> Result<Option<temps_workload::LogStream>, PlaneError> {
        let Some((handle, driver)) = self.latest_instance(id).await? else {
            return Ok(None);
        };
        driver
            .logs(
                &handle,
                temps_workload::LogQuery {
                    follow: true,
                    resume: true,
                    ..temps_workload::LogQuery::default()
                },
            )
            .await
            .map(Some)
            .map_err(|source| PlaneError::Driver {
                workload_id: id,
                operation: "follow the instance's logs",
                source,
            })
    }

    /// Workload `id`'s latest instance's logs under `query`, as the driver
    /// streams them (a following query keeps the stream open). `None` when it
    /// has no instance yet. For an interactive viewer; the ingesting reader
    /// uses [`Self::follow_instance_logs`].
    pub async fn instance_log_stream(
        &self,
        id: i64,
        query: temps_workload::LogQuery,
    ) -> Result<Option<temps_workload::LogStream>, PlaneError> {
        let Some((handle, driver)) = self.latest_instance(id).await? else {
            return Ok(None);
        };
        driver
            .logs(&handle, query)
            .await
            .map(Some)
            .map_err(|source| PlaneError::Driver {
                workload_id: id,
                operation: "stream the instance's logs",
                source,
            })
    }

    /// Run `command` in workload `id`'s latest instance and collect its
    /// output (bounded by the driver's exec). `None` when it has no
    /// instance yet.
    pub async fn instance_exec(
        &self,
        id: i64,
        exec: temps_workload::ExecSpec,
    ) -> Result<Option<Vec<temps_workload::LogLine>>, PlaneError> {
        use futures::TryStreamExt;
        let Some((handle, driver)) = self.latest_instance(id).await? else {
            return Ok(None);
        };
        let driver_error = |source| PlaneError::Driver {
            workload_id: id,
            operation: "exec in the instance",
            source,
        };
        let stream = driver.exec(&handle, exec).await.map_err(driver_error)?;
        let lines: Vec<temps_workload::LogLine> =
            stream.try_collect().await.map_err(driver_error)?;
        Ok(Some(lines))
    }

    /// The workload whose latest instance has runtime id `instance_id`, and
    /// the driver that runs it, so a product holding only the id (a
    /// deployment's `container_id`) can act on an instance the host's Docker
    /// cannot see (a microVM). `None` when no instance has that id.
    pub async fn workload_for_instance(
        &self,
        instance_id: &str,
    ) -> Result<Option<(i64, String)>, PlaneError> {
        let row = workload_instances::Entity::find()
            .filter(Expr::cust_with_values("handle->>'id' = $1", [instance_id]))
            .order_by_desc(workload_instances::Column::Id)
            .limit(1)
            .one(self.db.as_ref())
            .await
            .map_err(PlaneError::db(
                "finding the instance by its runtime id",
                format!("instance {instance_id}"),
            ))?;
        Ok(row.map(|row| (row.workload_id, row.driver)))
    }

    pub fn with_finalizer(mut self, finalizer: Arc<dyn Finalizer>) -> Self {
        self.finalizers
            .insert(finalizer.name().to_string(), finalizer);
        self
    }

    pub fn config(&self) -> &PlaneConfig {
        &self.config
    }

    /// Ask the reconciler for a pass.
    pub fn wake(&self) {
        self.notify.notify_one();
    }

    /// Start the reconciler. Only once per plane: a second call is refused,
    /// so a host never runs two reconcilers over the same objects.
    pub fn start(self: &Arc<Self>) -> Result<ReconcilerHandle, PlaneError> {
        if self.started.swap(true, Ordering::SeqCst) {
            return Err(PlaneError::ReconcilerAlreadyStarted {
                scope: self.config.scope.clone(),
            });
        }
        let task = tokio::spawn(crate::reconciler::run(Arc::clone(self)));
        Ok(ReconcilerHandle {
            task,
            plane: Arc::clone(self),
        })
    }

    pub(crate) fn abort_watchers(&self) {
        let mut watchers = self
            .watchers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for (_, watcher) in watchers.drain() {
            watcher.abort();
        }
    }

    /// Admit `request.spec` and store it. Returns the workload id.
    pub async fn apply(&self, request: ApplyRequest) -> Result<i64, PlaneError> {
        let ApplyRequest {
            owner,
            name,
            spec,
            project_slug,
            finalizers,
        } = request;
        let db = self.db.as_ref();
        let existing = workloads::Entity::find()
            .filter(workloads::Column::OwnerKind.eq(owner.kind.as_str()))
            .filter(workloads::Column::OwnerId.eq(owner.id))
            .filter(workloads::Column::Name.eq(name.as_str()))
            .filter(workloads::Column::DeletedAt.is_null())
            .one(db)
            .await
            .map_err(PlaneError::db("reading the workload to apply", &name))?;
        let active = workloads::Entity::find()
            .filter(workloads::Column::OwnerKind.eq(owner.kind.as_str()))
            .filter(workloads::Column::OwnerId.eq(owner.id))
            .filter(workloads::Column::Name.ne(name.as_str()))
            .filter(workloads::Column::DeletedAt.is_null())
            .filter(workloads::Column::Phase.is_not_in([
                WorkloadPhase::Succeeded.as_str(),
                WorkloadPhase::Failed.as_str(),
            ]))
            .count(db)
            .await
            .map_err(PlaneError::db(
                "counting the owner's active workloads",
                &name,
            ))?;

        let admitted = match admit(
            &self.config.admission,
            AdmissionInput {
                spec,
                name: &name,
                owner: &owner,
                project_slug: project_slug.as_deref(),
                active_for_owner: u32::try_from(active).unwrap_or(u32::MAX),
            },
        ) {
            Ok(spec) => spec,
            Err(refusal) => return Err(self.refuse(refusal).await),
        };
        let profile = admitted.security.map(|p| p.as_str()).unwrap_or("none");
        let spec_json = serde_json::to_value(&admitted).map_err(|e| PlaneError::Encode {
            workload: name.clone(),
            field: "spec",
            reason: e.to_string(),
        })?;

        if let Some(row) = existing {
            if row.spec == spec_json {
                return Ok(row.id);
            }
            workloads::Entity::update_many()
                .col_expr(workloads::Column::Spec, Expr::value(spec_json))
                .col_expr(
                    workloads::Column::Generation,
                    Expr::col(workloads::Column::Generation).add(1),
                )
                .filter(workloads::Column::Id.eq(row.id))
                .exec(db)
                .await
                .map_err(PlaneError::db("updating the workload spec", row.id))?;
            events::record(
                db,
                row.id,
                None,
                "SpecUpdated",
                &format!("generation {} admitted as {profile}", row.generation + 1),
                Utc::now(),
            )
            .await?;
            self.wake();
            return Ok(row.id);
        }

        let inserted = db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "INSERT INTO workloads (owner_kind, owner_id, name, spec, finalizers) \
                 VALUES ($1, $2, $3, $4, $5) \
                 ON CONFLICT (owner_kind, owner_id, name) WHERE deleted_at IS NULL DO NOTHING \
                 RETURNING id",
                vec![
                    owner.kind.as_str().into(),
                    owner.id.into(),
                    name.as_str().into(),
                    spec_json.into(),
                    finalizers.into(),
                ],
            ))
            .await
            .map_err(PlaneError::db("inserting the workload", &name))?;
        let id = match inserted {
            Some(row) => row
                .try_get::<i64>("", "id")
                .map_err(PlaneError::db("reading the inserted workload id", &name))?,
            // Another apply of the same name won the race: it is that one.
            None => workloads::Entity::find()
                .filter(workloads::Column::OwnerKind.eq(owner.kind.as_str()))
                .filter(workloads::Column::OwnerId.eq(owner.id))
                .filter(workloads::Column::Name.eq(name.as_str()))
                .filter(workloads::Column::DeletedAt.is_null())
                .one(db)
                .await
                .map_err(PlaneError::db(
                    "reading the concurrently applied workload",
                    &name,
                ))?
                .map(|row| row.id)
                .ok_or_else(|| PlaneError::Database {
                    operation: "inserting the workload",
                    workload: name.clone(),
                    source: sea_orm::DbErr::RecordNotInserted,
                })?,
        };
        events::record(
            db,
            id,
            None,
            "Admitted",
            &format!("{owner} workload '{name}' admitted as {profile}"),
            Utc::now(),
        )
        .await?;
        tracing::info!(workload_id = id, owner = %owner, name = %name, profile, "workload applied");
        self.wake();
        Ok(id)
    }

    /// Log and audit an admission refusal, and return it as the error.
    async fn refuse(&self, refusal: crate::error::AdmissionError) -> PlaneError {
        tracing::warn!(
            workload = %refusal.workload,
            owner = %refusal.owner,
            rule = %refusal.rule,
            "workload refused at admission: {}",
            refusal.message
        );
        if let Err(error) = self
            .audit
            .create_audit_log(&AdmissionRefusedAudit::from(&refusal))
            .await
        {
            tracing::error!(
                workload = %refusal.workload,
                "failed to audit-log an admission refusal: {error}"
            );
        }
        refusal.into()
    }

    /// Apply a spec that mounts volumes of `volume_owner` (ADR-055 §12).
    ///
    /// The mounts are checked against the owner's volume rows (names, paths,
    /// the driver's volume kinds); the volumes pin the workload to their
    /// node (§8), which replaces `spec.placement.nodes` and is recorded as a
    /// `PinnedByVolume` event. A volume with no node yet is created on the
    /// node the spec is pinned to (it must name exactly one), through the
    /// volume provider on the control plane, and bound there. The plane, not
    /// the product, sets each mount's bind source, so a spec can never bind
    /// a host path. Any refusal is an audited [`AdmissionRule::Validation`].
    ///
    /// A spec without mounts is a plain [`WorkloadPlane::apply`].
    ///
    /// [`AdmissionRule::Validation`]: crate::AdmissionRule::Validation
    pub async fn apply_with_volumes(
        &self,
        mut request: ApplyRequest,
        volume_owner: &Owner,
    ) -> Result<VolumeApplied, PlaneError> {
        if request.spec.volumes.is_empty() {
            return Ok(VolumeApplied {
                workload_id: self.apply(request).await?,
                pin: None,
            });
        }
        let workload_name = request.name.clone();
        let owner_text = request.owner.to_string();
        let refusal = |message: String| crate::error::AdmissionError {
            rule: crate::AdmissionRule::Validation,
            workload: workload_name.clone(),
            owner: owner_text.clone(),
            message,
        };
        let store = VolumeStore::new(self.db.clone());
        let records = store
            .all_of(volume_owner)
            .await
            .map_err(|source| PlaneError::Volume {
                workload: request.name.clone(),
                operation: "read",
                source: Box::new(source),
            })?;
        let spec_owner = request.spec.owner;
        let pin = match pin_for(
            spec_owner,
            &request.spec.volumes,
            &records,
            request.spec.placement.pinned_nodes(),
        ) {
            Ok(pin) => pin,
            Err(error) => {
                let refused = refusal(format!("volumes of {volume_owner}: {error}"));
                return Err(self.refuse(refused).await);
            }
        };
        let node_id = match (&pin, request.spec.placement.pinned_nodes()) {
            (Some(pin), _) => pin.node_id,
            (None, Some([node])) => *node,
            (None, _) => {
                let names: Vec<&str> = request
                    .spec
                    .volumes
                    .iter()
                    .map(|m| m.volume.as_str())
                    .collect();
                let refused = refusal(format!(
                    "volume(s) {names:?} of {volume_owner} have not been created on any node \
                     yet, and the workload is not pinned to exactly one node to create them on"
                ));
                return Err(self.refuse(refused).await);
            }
        };
        request.spec.placement.nodes = Some(vec![node_id]);
        let mut names = Vec::with_capacity(request.spec.volumes.len());
        for mount in &mut request.spec.volumes {
            // `pin_for` resolved every mount, so the record exists.
            let Some(record) = records.iter().find(|r| r.name == mount.volume) else {
                continue;
            };
            if node_id == scheduler::CONTROL_PLANE_NODE_ID {
                if let Some(provider) = self.volume_provider.as_ref() {
                    provider
                        .ensure(&record.as_volume(spec_owner))
                        .await
                        .map_err(|source| PlaneError::VolumeProvider {
                            workload: workload_name.clone(),
                            volume: record.name.clone(),
                            node_id,
                            source: Box::new(source),
                        })?;
                }
            }
            if record.node_id.is_none() {
                let bound = store
                    .bind_node(volume_owner, record.id, node_id)
                    .await
                    .map_err(|source| PlaneError::Volume {
                        workload: workload_name.clone(),
                        operation: "bound to their node",
                        source: Box::new(source),
                    })?;
                if bound != node_id {
                    let refused = refusal(format!(
                        "volume '{}' of {volume_owner} was created on node {bound} while this \
                         workload was being placed on node {node_id}; apply it again",
                        record.name
                    ));
                    return Err(self.refuse(refused).await);
                }
            }
            mount.source = Some(bind_source(&record.backing).to_string());
            names.push(record.name.clone());
        }
        names.sort();
        let pin = VolumePin {
            node_id,
            volumes: names,
        };
        let workload_id = self.apply(request).await?;
        events::record(
            self.db.as_ref(),
            workload_id,
            None,
            "PinnedByVolume",
            &pin.to_string(),
            Utc::now(),
        )
        .await?;
        Ok(VolumeApplied {
            workload_id,
            pin: Some(pin),
        })
    }

    /// Apply, then wait until `condition` is `True`. A workload that finishes
    /// (or is deleted) first returns [`PlaneError::ConditionFailed`]; the
    /// timeout returns [`PlaneError::WaitTimedOut`]. Both carry the
    /// condition's reason and message.
    pub async fn apply_and_wait(
        &self,
        request: ApplyRequest,
        condition: ConditionKind,
        timeout: Duration,
    ) -> Result<WorkloadView, PlaneError> {
        let id = self.apply(request).await?;
        self.wait_for(id, condition, timeout).await
    }

    /// Wait until `condition` of workload `id` is `True`.
    pub async fn wait_for(
        &self,
        id: i64,
        condition: ConditionKind,
        timeout: Duration,
    ) -> Result<WorkloadView, PlaneError> {
        let started = tokio::time::Instant::now();
        let deadline = started + timeout;
        let mut changes = self.status_tx.subscribe();
        loop {
            let view = self.get(id).await?;
            let current = view.status.condition(condition);
            if current.is_some_and(|c| c.status == ConditionStatus::True) {
                return Ok(view);
            }
            let (status, reason, message) = condition_explanation(&view.status, condition);
            if view.status.phase.is_terminal() || view.deleted_at.is_some() {
                return Err(PlaneError::ConditionFailed {
                    workload_id: id,
                    name: view.name,
                    condition,
                    status,
                    reason,
                    message,
                });
            }
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return Err(PlaneError::WaitTimedOut {
                    workload_id: id,
                    name: view.name,
                    condition,
                    waited_ms: u64::try_from(now.duration_since(started).as_millis())
                        .unwrap_or(u64::MAX),
                    reason,
                    message,
                });
            }
            // A status write wakes us; the poll bounds a write made by
            // another process.
            let wait = (deadline - now).min(Duration::from_secs(1));
            match tokio::time::timeout(wait, changes.changed()).await {
                Ok(Ok(())) | Err(_) => {}
                Ok(Err(_)) => tokio::time::sleep(wait).await,
            }
        }
    }

    /// The workload `id`, deleted or not.
    pub async fn get(&self, id: i64) -> Result<WorkloadView, PlaneError> {
        let row = workloads::Entity::find_by_id(id)
            .one(self.db.as_ref())
            .await
            .map_err(PlaneError::db("reading the workload", id))?
            .ok_or(PlaneError::NotFound { workload_id: id })?;
        let latest = workload_instances::Entity::find()
            .filter(workload_instances::Column::WorkloadId.eq(id))
            .order_by_desc(workload_instances::Column::Attempt)
            .limit(1)
            .one(self.db.as_ref())
            .await
            .map_err(PlaneError::db("reading the latest instance", id))?;
        view_from(row, latest.as_ref())
    }

    /// Live workloads of `owner` that have not finished, oldest first.
    pub async fn list_active(&self, owner: &Owner) -> Result<Vec<WorkloadView>, PlaneError> {
        let rows = workloads::Entity::find()
            .filter(workloads::Column::OwnerKind.eq(owner.kind.as_str()))
            .filter(workloads::Column::OwnerId.eq(owner.id))
            .filter(workloads::Column::DeletedAt.is_null())
            .filter(workloads::Column::Phase.is_not_in([
                WorkloadPhase::Succeeded.as_str(),
                WorkloadPhase::Failed.as_str(),
            ]))
            .order_by_asc(workloads::Column::Id)
            .all(self.db.as_ref())
            .await
            .map_err(PlaneError::db(
                "listing the owner's active workloads",
                owner,
            ))?;
        rows.into_iter().map(|row| view_from(row, None)).collect()
    }

    /// The workload's events, newest first.
    pub async fn events(&self, id: i64) -> Result<Vec<temps_workload::WorkloadEvent>, PlaneError> {
        events::list(self.db.as_ref(), id).await
    }

    /// The attachments for an instance created on the control plane: the
    /// template's, with the cluster resolver filled in when one is set.
    pub(crate) fn local_attachments(
        &self,
        attachments: &temps_workload::Attachments,
    ) -> temps_workload::Attachments {
        let mut attachments = attachments.clone();
        if let Some(dns) = &self.config.cluster_dns {
            dns.apply(&mut attachments.network);
        }
        attachments
    }

    /// A guard that retires workload `id` with `reason` and `message` when
    /// dropped, unless [`RetireOnDrop::disarm`]ed.
    pub fn retire_on_drop(
        self: &Arc<Self>,
        id: i64,
        reason: &'static str,
        message: impl Into<String>,
    ) -> RetireOnDrop {
        RetireOnDrop {
            plane: self.clone(),
            workload_id: id,
            reason,
            message: message.into(),
            armed: true,
        }
    }

    /// Delete workload `id`: its instances are stopped and removed, its
    /// finalizers run, then the row goes. An unfinished workload fails
    /// with `reason`. Deleting a deleted workload is a no-op.
    pub async fn delete(&self, id: i64, reason: &str, message: &str) -> Result<(), PlaneError> {
        let db = self.db.as_ref();
        let row = workloads::Entity::find_by_id(id)
            .one(db)
            .await
            .map_err(PlaneError::db("reading the workload to delete", id))?
            .ok_or(PlaneError::NotFound { workload_id: id })?;
        if row.deleted_at.is_some() {
            return Ok(());
        }
        let now = Utc::now();
        let mut status = decode_status(&row)?;
        if !status.phase.is_terminal() {
            status.set_condition(
                ConditionKind::Completed,
                ConditionStatus::False,
                reason,
                message,
                now,
            );
            status.ready_instances = 0;
            status.phase = WorkloadPhase::Terminating;
        }
        let status_json = encode_status(&row.name, &status)?;
        workloads::Entity::update_many()
            .col_expr(workloads::Column::DeletedAt, Expr::value(now))
            .col_expr(workloads::Column::Status, Expr::value(status_json))
            .col_expr(workloads::Column::Phase, Expr::value(status.phase.as_str()))
            .filter(workloads::Column::Id.eq(id))
            .exec(db)
            .await
            .map_err(PlaneError::db("marking the workload deleted", id))?;
        events::record(db, id, None, reason, message, now).await?;
        self.status_tx.send_modify(|seq| *seq += 1);
        self.wake();
        Ok(())
    }
}

/// The condition's status, reason and message; when it is not set, the
/// first `False` condition, which is usually why.
fn condition_explanation(
    status: &WorkloadStatus,
    condition: ConditionKind,
) -> (ConditionStatus, String, String) {
    if let Some(c) = status.condition(condition) {
        return (c.status, c.reason.clone(), c.message.clone());
    }
    if let Some(c) = status
        .conditions
        .iter()
        .find(|c| c.status == ConditionStatus::False)
    {
        return (
            ConditionStatus::Unknown,
            c.reason.clone(),
            format!("{:?} is False: {}", c.kind, c.message),
        );
    }
    (
        ConditionStatus::Unknown,
        "NotObserved".to_string(),
        format!("the plane has not reported {condition:?} yet"),
    )
}

pub(crate) const NO_CONTROL_PLANE_DRIVER: &str = "0/1 nodes eligible: the control plane (node 0) \
     has no runtime driver; the Docker daemon was not reachable when temps started";

/// Why node 0 cannot run a workload requesting `isolation`.
pub(crate) fn no_driver_for_isolation(isolation: Isolation, drivers: &[String]) -> String {
    let wanted = scheduler::isolation_name(isolation);
    let have = if drivers.is_empty() {
        "none".to_string()
    } else {
        drivers.join(", ")
    };
    match isolation {
        Isolation::MicroVm => format!(
            "0/1 nodes eligible: the control plane (node 0) has no runtime that provides \
             {wanted} isolation (drivers: {have}). Set up Firecracker on this host (`sudo temps \
             firecracker setup`) and restart temps, or set the environment's workload isolation \
             back to process"
        ),
        _ => format!(
            "0/1 nodes eligible: the control plane (node 0) has no runtime that provides \
             {wanted} isolation (drivers: {have})"
        ),
    }
}

pub(crate) fn decode_status(row: &workloads::Model) -> Result<WorkloadStatus, PlaneError> {
    serde_json::from_value(row.status.clone()).map_err(|e| PlaneError::Corrupt {
        workload_id: row.id,
        field: "status",
        reason: e.to_string(),
    })
}

pub(crate) fn encode_status(
    name: &str,
    status: &WorkloadStatus,
) -> Result<serde_json::Value, PlaneError> {
    serde_json::to_value(status).map_err(|e| PlaneError::Encode {
        workload: name.to_string(),
        field: "status",
        reason: e.to_string(),
    })
}

pub(crate) fn view_from(
    row: workloads::Model,
    latest: Option<&workload_instances::Model>,
) -> Result<WorkloadView, PlaneError> {
    let spec: WorkloadSpec =
        serde_json::from_value(row.spec.clone()).map_err(|e| PlaneError::Corrupt {
            workload_id: row.id,
            field: "spec",
            reason: e.to_string(),
        })?;
    let status = decode_status(&row)?;
    Ok(WorkloadView {
        id: row.id,
        owner: Owner::new(row.owner_kind, row.owner_id),
        name: row.name,
        spec,
        status,
        generation: row.generation,
        observed_generation: row.observed_generation,
        change_seq: row.change_seq,
        finalizers: row.finalizers,
        created_at: row.created_at,
        deleted_at: row.deleted_at,
        expires_at: row.expires_at,
        attempt: latest
            .map(|i| u32::try_from(i.attempt).unwrap_or(0))
            .unwrap_or(0),
        finished_at: latest.and_then(|i| i.finished_at),
        output_tail: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{
        apply_request, deployment_spec, job_spec, job_spec_with, plane_with, test_db,
    };

    /// One enforcing worker with exactly 512 MB of headroom. Reading it
    /// takes a moment, as a database read does, so two placements started
    /// together both read the facts before either decides.
    struct OneSlot;

    #[async_trait]
    impl NodeInventory for OneSlot {
        async fn nodes(&self, _: Option<&str>) -> Result<Vec<scheduler::NodeFacts>, String> {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let mut node = scheduler::NodeFacts::control_plane(Some("linux/amd64".into()));
            node.id = 9;
            node.name = "worker-9".into();
            node.resource_policy =
                temps_entities::node_resource_settings::NodeResourcePolicy::Enforced;
            node.system_reserved = temps_entities::node_resource_settings::SystemReserved::NONE;
            node.usage = Some(scheduler::NodeUsage {
                cpu_percent: Some(0.0),
                memory_used_bytes: Some(0.0),
                memory_total_bytes: Some(512.0 * 1024.0 * 1024.0),
                cpu_cores: Some(4.0),
            });
            Ok(vec![node])
        }

        fn socket_gate(&self, _: Option<&str>) -> Option<scheduler::SocketGate> {
            None
        }

        async fn explain_context(
            &self,
            _: &PlacementHints,
            _: Option<&str>,
        ) -> Result<scheduler::ExplainContext, String> {
            Ok(Default::default())
        }
    }

    /// ADR-055 §5: two placements racing for the last 512 MB of a node:
    /// exactly one is admitted, the other sees the first's reservation.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_placements_cannot_both_take_the_last_slot() {
        let plane = Arc::new(
            WorkloadPlane::new(
                Arc::new(DatabaseConnection::Disconnected),
                NodeDriver(None),
                Arc::new(crate::NoEnvSource),
                Arc::new(crate::testing::RecordingAudit::default()),
                PlaneConfig::new("t-slot"),
            )
            .with_inventory(Arc::new(OneSlot)),
        );
        let mut spec = deployment_spec("app:1");
        spec.resources.requests = Some(temps_workload::ResourceAmounts {
            cpu_millis: Some(100),
            memory_mb: Some(512),
            ..Default::default()
        });
        let hints = PlacementHints {
            exclude_control_plane: true,
            ..Default::default()
        };
        let race = |name: &'static str| {
            let plane = plane.clone();
            let spec = spec.clone();
            let hints = hints.clone();
            tokio::spawn(async move { plane.schedule(name, &spec, &hints).await })
        };
        let (first, second) = (race("replica-a"), race("replica-b"));
        let outcomes = [first.await.expect("joined"), second.await.expect("joined")];
        let admitted: Vec<_> = outcomes.iter().filter_map(|o| o.as_ref().ok()).collect();
        assert_eq!(
            admitted.len(),
            1,
            "exactly one placement fits: {outcomes:?}"
        );
        assert_eq!(admitted[0].0.nodes, vec![9]);
        assert!(
            outcomes
                .iter()
                .any(|o| matches!(o, Err(PlaneError::Unschedulable { .. }))),
            "the other is refused for capacity: {outcomes:?}"
        );
        // The held reservation is what refused it: once released, the
        // slot is free again.
        drop(outcomes);
        assert!(plane.in_flight.by_node().is_empty());
        assert!(plane.schedule("replica-c", &spec, &hints).await.is_ok());
    }

    #[tokio::test]
    async fn an_abandoned_handoff_retires_its_workload_and_a_disarmed_one_keeps_it() {
        let Some(db) = test_db().await else { return };
        let (plane, driver, _audit) = plane_with(&db);
        let abandoned = plane
            .apply(apply_request(31, "replica-a", deployment_spec("app:1")))
            .await
            .expect("applied");
        let kept = plane
            .apply(apply_request(32, "replica-b", deployment_spec("app:1")))
            .await
            .expect("applied");
        plane.reconcile_at(Utc::now(), false).await;
        assert_eq!(driver.running().len(), 2);
        let abandoned_instance = driver
            .running()
            .into_iter()
            .find(|name| name.ends_with(&format!("-{abandoned}-a1")))
            .expect("the abandoned replica's instance");

        // The caller gave up (its wait failed or it was cancelled).
        drop(plane.retire_on_drop(abandoned, "HandoffFailed", "the wait timed out"));
        plane
            .retire_on_drop(kept, "HandoffFailed", "unused")
            .disarm();
        let mut deleted = false;
        for _ in 0..100 {
            if plane
                .get(abandoned)
                .await
                .map_or(true, |view| view.deleted_at.is_some())
            {
                deleted = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(deleted, "the abandoned workload is deleted");
        plane.reconcile_at(Utc::now(), false).await;
        assert!(
            !driver.exists(&abandoned_instance),
            "and its container removed"
        );
        let view = plane.get(kept).await.expect("the disarmed workload stays");
        assert!(view.deleted_at.is_none());
        assert_eq!(driver.running().len(), 1);
    }

    #[tokio::test]
    async fn applying_the_same_spec_twice_is_one_workload() {
        let Some(db) = test_db().await else { return };
        let (plane, _driver, _audit) = plane_with(&db);
        let first = plane
            .apply(apply_request(1, "run-a", job_spec("app:1")))
            .await
            .expect("applied");
        let again = plane
            .apply(apply_request(1, "run-a", job_spec("app:1")))
            .await
            .expect("applied");
        assert_eq!(first, again);
        let view = plane.get(first).await.expect("read");
        assert_eq!(view.generation, 1);

        let third = plane
            .apply(apply_request(1, "run-a", job_spec("app:2")))
            .await
            .expect("applied");
        assert_eq!(third, first);
        let view = plane.get(first).await.expect("read");
        assert_eq!(view.generation, 2);
        assert!(view.change_seq > 0);
    }

    #[tokio::test]
    async fn a_refusal_is_typed_and_audit_logged() {
        let Some(db) = test_db().await else { return };
        let (plane, _driver, audit) = plane_with(&db);
        let mut spec = job_spec("app:1");
        spec.security = Some(temps_workload::SecurityProfile::HostPrivileged);
        let error = plane
            .apply(apply_request(2, "run-b", spec))
            .await
            .expect_err("refused");
        let PlaneError::Admission(refusal) = error else {
            panic!("expected an admission error, got {error}");
        };
        assert_eq!(refusal.rule, crate::AdmissionRule::SecurityProfile);
        assert_eq!(refusal.workload, "run-b");
        let logged = audit.operations();
        assert_eq!(logged.len(), 1);
        assert!(
            logged[0].starts_with("WORKLOAD_ADMISSION_REFUSED"),
            "{}",
            logged[0]
        );
        assert!(logged[0].contains("security_profile"), "{}", logged[0]);
    }

    #[tokio::test]
    async fn the_quota_counts_the_owners_unfinished_workloads() {
        let Some(db) = test_db().await else { return };
        let (plane, _driver, _audit) = plane_with(&db);
        for i in 0..5 {
            plane
                .apply(apply_request(4, &format!("run-{i}"), job_spec("app:1")))
                .await
                .expect("applied");
        }
        let error = plane
            .apply(apply_request(4, "run-5", job_spec("app:1")))
            .await
            .expect_err("over quota");
        assert!(
            matches!(&error, PlaneError::Admission(r) if r.rule == crate::AdmissionRule::Quota),
            "{error}"
        );
        // Another owner is not affected.
        plane
            .apply(apply_request(5, "run-0", job_spec("app:1")))
            .await
            .expect("applied");
    }

    #[tokio::test]
    async fn apply_and_wait_fails_with_the_reason_of_a_finished_run() {
        let Some(db) = test_db().await else { return };
        let (plane, driver, _audit) = plane_with(&db);
        driver.fail_prepare(true);
        let id = plane
            .apply(apply_request(3, "run-c", job_spec("app:1")))
            .await
            .expect("applied");
        plane.reconcile_at(Utc::now(), false).await;
        let error = plane
            .wait_for(id, ConditionKind::Ready, Duration::from_millis(200))
            .await
            .expect_err("failed");
        match error {
            PlaneError::ConditionFailed {
                reason, message, ..
            } => {
                assert_eq!(reason, "ArtifactPullFailed");
                assert!(message.contains("app:1"), "{message}");
            }
            other => panic!("expected ConditionFailed, got {other}"),
        }
    }

    #[tokio::test]
    async fn apply_and_wait_times_out_with_the_condition_reason() {
        let Some(db) = test_db().await else { return };
        let (plane, driver, _audit) = plane_with(&db);
        let id = plane
            .apply(apply_request(
                6,
                "run-d",
                job_spec_with("app:1", 600, 1, None),
            ))
            .await
            .expect("applied");
        plane.reconcile_at(Utc::now(), false).await;
        let running = driver.running();
        driver.exit(&running[0], 1);
        // Observed; the retry waits for its back-off.
        plane.reconcile_at(Utc::now(), false).await;
        let error = plane
            .wait_for(id, ConditionKind::Running, Duration::from_millis(150))
            .await
            .expect_err("timed out");
        let PlaneError::WaitTimedOut {
            reason,
            message,
            condition,
            workload_id,
            ..
        } = error
        else {
            panic!("expected WaitTimedOut, got {error}");
        };
        assert_eq!(workload_id, id);
        assert_eq!(condition, ConditionKind::Running);
        assert_eq!(reason, "BackOff");
        assert!(message.contains("attempt 1 exited 1"), "{message}");

        // A condition never reported says so.
        let pending = plane
            .apply(apply_request(6, "run-e", job_spec("app:1")))
            .await
            .expect("applied");
        let error = plane
            .apply_and_wait(
                apply_request(6, "run-e", job_spec("app:1")),
                ConditionKind::Completed,
                Duration::from_millis(100),
            )
            .await
            .expect_err("timed out");
        assert!(
            matches!(&error, PlaneError::WaitTimedOut { reason, workload_id, .. }
                if reason == "NotObserved" && *workload_id == pending),
            "{error}"
        );
    }
}
