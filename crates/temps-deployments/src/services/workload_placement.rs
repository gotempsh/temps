// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Deployments onto the workload plane: placement (ADR-055 §5, §8, Phase 5).
//!
//! The product side of the unified scheduler. It owns what the plane must
//! not read itself (the `nodes` table, the control plane's node-0 settings,
//! the containers already running) and turns it into [`NodeFacts`]; maps a
//! project's and environment's `target_nodes` / `target_labels` /
//! `anti_affinity` and resource limits onto a [`PlacementSpec`] and
//! [`Resources`]; and holds the operator-controlled modes:
//!
//! - `environments.workload_mode` (`legacy` | `shadow` | `enforce`);
//! - `nodes.resource_policy` and `nodes.system_reserved`, and the control
//!   plane's own pair in the instance settings (node 0 has no row).
//!
//! The shadow comparison ([`compare_placements`]) is the Phase 5 proof: the
//! same request is decided by `NodeScheduler` and by the unified scheduler,
//! and every disagreement is either explained by a documented, intended
//! difference ([`ExpectedDifference`]) or reported as unexpected.
//!
//! Everything here is control plane: one query per table per call, no
//! per-request work.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, DatabaseConnection, DbBackend, DbErr, EntityTrait,
    FromQueryResult, Statement,
};
use serde::Serialize;
use temps_core::docker_socket_grant::DockerSocketGrant;
use temps_entities::deployment_config::DeploymentConfig;
use temps_entities::environment_workload_isolation::{self, EnvironmentWorkloadIsolation};
use temps_entities::environment_workload_mode::EnvironmentWorkloadMode;
use temps_entities::node_resource_settings::{self, NodeResourcePolicy, SystemReserved};
use temps_entities::{environments, nodes, projects};
use temps_workload::{
    Isolation, LabelSelector, LabelValues, PlacementSpec, ResourceAmounts, Resources, WorkloadKind,
};
use temps_workload_plane::inventory::{NodeInventory, PlacementHints};
use temps_workload_plane::scheduler::{NodeRole, CONTROL_PLANE_NODE_ID, ROLE_LABEL};
use temps_workload_plane::{
    ExplainContext, Explanation, NodeFacts, NodeUsage, PinningRecommendation, Placement,
    Reservation, ScheduleError, ScheduleRequest, SchedulerConfig, SocketGate, UnifiedScheduler,
};

use super::node_scheduler::{NodeAssignment, NodeScheduler, ReplicaPlacementRequest};
use super::node_service::{NodeError, NodeService};

/// Largest reserve accepted: 256 cores, 4 TB. A typo beyond this would make
/// a node unschedulable without saying why.
const MAX_RESERVED_CPU_MILLIS: u32 = 256_000;
const MAX_RESERVED_MEMORY_MB: u32 = 4 * 1024 * 1024;
/// Window the pinning recommendation looks back over (ADR-055 §8: "moved
/// between 3 nodes in 7 days").
pub const DEFAULT_RECOMMENDATION_WINDOW_DAYS: u32 = 7;

#[derive(Debug, thiserror::Error)]
pub enum WorkloadPlacementError {
    #[error("Environment {environment_id} not found")]
    EnvironmentNotFound { environment_id: i32 },

    #[error("Project {project_id} of environment {environment_id} not found")]
    ProjectNotFound {
        project_id: i32,
        environment_id: i32,
    },

    #[error("Node {node_id} not found")]
    NodeNotFound { node_id: i32 },

    #[error("Invalid placement settings: {message}")]
    Validation { message: String },

    #[error("Database error while {operation}: {source}")]
    Database {
        operation: &'static str,
        #[source]
        source: DbErr,
    },

    #[error("Reading nodes failed while {operation}: {source}")]
    Node {
        operation: &'static str,
        #[source]
        source: NodeError,
    },

    #[error("The control plane's (node 0) settings could not be {operation}: {reason}")]
    Settings {
        operation: &'static str,
        reason: String,
    },
}

fn db_error(operation: &'static str) -> impl FnOnce(DbErr) -> WorkloadPlacementError {
    move |source| WorkloadPlacementError::Database { operation, source }
}

fn node_error(operation: &'static str) -> impl FnOnce(NodeError) -> WorkloadPlacementError {
    move |source| WorkloadPlacementError::Node { operation, source }
}

// ── Mapping today's settings onto the plane's types ─────────────────────────

/// The `target_labels` JSON selector as a [`LabelSelector`], compared as
/// strings exactly as `NodeScheduler` compares them (`"true"` and `true` are
/// the same value). `None` for an absent or empty selector.
pub fn label_selector_from_json(
    selector: Option<&serde_json::Value>,
) -> Result<Option<LabelSelector>, WorkloadPlacementError> {
    let Some(selector) = selector else {
        return Ok(None);
    };
    let map = selector
        .as_object()
        .ok_or_else(|| WorkloadPlacementError::Validation {
            message: format!("target_labels must be a JSON object, got {selector}"),
        })?;
    if map.is_empty() {
        return Ok(None);
    }
    let text = |value: &serde_json::Value| match value {
        serde_json::Value::String(text) => text.clone(),
        other => other.to_string().trim_matches('"').to_string(),
    };
    Ok(Some(
        map.iter()
            .map(|(key, value)| {
                let values = match value.as_array() {
                    Some(values) => LabelValues::AnyOf(values.iter().map(text).collect()),
                    None => LabelValues::One(text(value)),
                };
                (key.clone(), values)
            })
            .collect(),
    ))
}

/// A node's labels as strings (`NodeScheduler` stringifies non-string
/// values the same way).
fn node_labels(labels: &serde_json::Value) -> BTreeMap<String, String> {
    labels
        .as_object()
        .map(|map| {
            map.iter()
                .map(|(key, value)| {
                    let value = match value {
                        serde_json::Value::String(text) => text.clone(),
                        other => other.to_string().trim_matches('"').to_string(),
                    };
                    (key.clone(), value)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The environment's placement: `target_nodes` and `target_labels` each
/// inherit the project's when the environment does not set them, and
/// `anti_affinity` comes from the environment's config when it has one (the
/// deploy path's resolution, unchanged). Pins carry over as a strict
/// [`PlacementSpec`].
pub fn placement_spec_for(
    environment: Option<&DeploymentConfig>,
    project: Option<&DeploymentConfig>,
    replicas: u32,
) -> Result<PlacementSpec, WorkloadPlacementError> {
    let configs = [environment, project];
    let nodes = configs
        .iter()
        .flatten()
        .find_map(|config| config.configured_target_nodes().map(<[i32]>::to_vec));
    let labels = configs
        .iter()
        .flatten()
        .find_map(|config| config.configured_target_labels());
    let spread = environment
        .map(|config| config.anti_affinity)
        .or_else(|| project.map(|config| config.anti_affinity))
        .unwrap_or(true);
    Ok(PlacementSpec {
        nodes,
        labels: label_selector_from_json(labels)?,
        spread,
        replicas: replicas.max(1),
    })
}

/// The job config's placement (already resolved env-over-project by the
/// workflow) as a [`PlacementSpec`].
pub fn placement_spec_from_job(
    target_nodes: Option<&[i32]>,
    target_labels: Option<&serde_json::Value>,
    anti_affinity: bool,
    replicas: u32,
) -> Result<PlacementSpec, WorkloadPlacementError> {
    Ok(PlacementSpec {
        nodes: target_nodes
            .filter(|nodes| !nodes.is_empty())
            .map(<[i32]>::to_vec),
        labels: label_selector_from_json(target_labels)?,
        spread: anti_affinity,
        replicas: replicas.max(1),
    })
}

/// Microcores (the stored unit, 1_000_000 = one core) to millicores,
/// rounded up so a configured limit is never loosened.
fn micro_to_millis(micro: i32) -> Option<u32> {
    (micro > 0).then(|| (micro as u32).div_ceil(1000))
}

/// MB, where `0` means explicitly uncapped (no limit).
fn positive_mb(mb: i32) -> Option<u32> {
    (mb > 0).then_some(mb as u32)
}

/// The environment's (else the project's) CPU and memory settings: the
/// current limits are carried over as `Resources.limits`, requests as
/// requests. Every field is resolved on its own, as the deploy path does.
pub fn resources_for(
    environment: Option<&DeploymentConfig>,
    project: Option<&DeploymentConfig>,
) -> Resources {
    let pick = |getter: fn(&DeploymentConfig) -> Option<i32>| -> Option<i32> {
        environment
            .and_then(getter)
            .or_else(|| project.and_then(getter))
    };
    let limits = ResourceAmounts {
        cpu_millis: pick(|c| c.cpu_limit).and_then(micro_to_millis),
        memory_mb: pick(|c| c.memory_limit).and_then(positive_mb),
        disk_mb: None,
        pids: None,
    };
    let requests = ResourceAmounts {
        cpu_millis: pick(|c| c.cpu_request).and_then(micro_to_millis),
        memory_mb: pick(|c| c.memory_request).and_then(positive_mb),
        disk_mb: None,
        pids: None,
    };
    Resources {
        requests: (!requests.is_empty()).then_some(requests),
        limits: (!limits.is_empty()).then_some(limits),
    }
}

/// The scheduler's view of one `nodes` row.
pub fn node_facts_from_row(
    node: &nodes::Model,
    settings: Option<&node_resource_settings::Model>,
    committed: Option<&ContainerLoad>,
    grants_socket: bool,
    now: DateTime<Utc>,
    heartbeat_threshold_secs: i64,
) -> NodeFacts {
    let cutoff = now - chrono::Duration::seconds(heartbeat_threshold_secs);
    let labels = node_labels(&node.labels);
    NodeFacts {
        id: node.id,
        name: node.name.clone(),
        status: node.status.clone(),
        heartbeat_fresh: node.last_heartbeat.is_some_and(|at| at >= cutoff),
        role: NodeRole::from_label(labels.get(ROLE_LABEL).map(String::as_str)),
        labels,
        architecture: node.architecture.clone(),
        // Every node runs Docker today: process isolation.
        isolations: Vec::new(),
        grants_socket,
        usage: NodeUsage::from_capacity_json(&node.capacity),
        resource_policy: settings.map(|s| s.resource_policy).unwrap_or_default(),
        system_reserved: settings
            .map(|s| s.system_reserved)
            .unwrap_or_else(|| SystemReserved::default_for_role(&node.role)),
        committed: committed.map(|load| load.reservation).unwrap_or_default(),
        in_flight: Reservation::default(),
        live_instances: committed.map(|load| load.instances).unwrap_or(0),
    }
}

/// What the containers running on one node hold.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ContainerLoad {
    pub reservation: Reservation,
    pub instances: u32,
}

// ── The shadow comparison ───────────────────────────────────────────────────

/// One scheduler's answer: where each replica went (`0` = the control
/// plane), or the reason code it refused with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "outcome")]
pub enum ShadowOutcome {
    Placed { nodes: Vec<i32> },
    Refused { reason: String },
}

impl ShadowOutcome {
    pub fn from_legacy(
        result: &Result<super::node_scheduler::SchedulingOutcome, NodeError>,
    ) -> Self {
        match result {
            Ok(outcome) => ShadowOutcome::Placed {
                nodes: outcome
                    .assignments
                    .iter()
                    .map(|assignment| match assignment {
                        NodeAssignment::Local => CONTROL_PLANE_NODE_ID,
                        NodeAssignment::Remote { node_id, .. } => *node_id,
                    })
                    .collect(),
            },
            Err(error) => ShadowOutcome::Refused {
                reason: legacy_reason(error).to_string(),
            },
        }
    }

    pub fn from_unified(result: &Result<Placement, ScheduleError>) -> Self {
        match result {
            Ok(placement) => ShadowOutcome::Placed {
                nodes: placement.nodes.clone(),
            },
            Err(error) => ShadowOutcome::Refused {
                reason: error.reason().to_string(),
            },
        }
    }
}

/// `NodeScheduler`'s error as the unified scheduler's `Scheduled` reason.
pub fn legacy_reason(error: &NodeError) -> &'static str {
    match error {
        NodeError::NoCompatibleNode { .. } => "NoCompatibleNode",
        NodeError::InsufficientCompatibleNodes { .. } => "InsufficientCompatibleNodes",
        NodeError::PlacementConstraintsUnsatisfied { .. } => "PlacementConstraintsUnsatisfied",
        NodeError::DedicatedNodesNotPinned { .. } => "DedicatedNodesNotPinned",
        NodeError::LocalWorkloadsDisabled { .. } => "LocalWorkloadsDisabled",
        NodeError::DockerSocketNotSchedulable { .. } => "DockerSocketNotSchedulable",
        NodeError::Validation { .. } => "Validation",
        _ => "SchedulerFailed",
    }
}

/// A disagreement ADR-055 intends, proven by re-running the unified
/// scheduler without the difference and getting `NodeScheduler`'s answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExpectedDifference {
    /// §5: the load threshold applies to capacity minus `system_reserved`,
    /// which `NodeScheduler` does not deduct.
    SystemReserved,
    /// §5: an `enforced` node refuses what exceeds its schedulable capacity
    /// (`NodeScheduler` never counts reservations).
    EnforcedCapacity,
    /// §8: placements in flight count against a node; `NodeScheduler` only
    /// sees load once a heartbeat reports it.
    InFlightPlacements,
    /// Neither scheduler has load to rank by and each keeps its own
    /// round-robin cursor; both placed the same number of replicas onto
    /// nodes the other also found eligible.
    RoundRobinCursor,
}

impl ExpectedDifference {
    pub fn as_str(&self) -> &'static str {
        match self {
            ExpectedDifference::SystemReserved => "system_reserved",
            ExpectedDifference::EnforcedCapacity => "enforced_capacity",
            ExpectedDifference::InFlightPlacements => "in_flight_placements",
            ExpectedDifference::RoundRobinCursor => "round_robin_cursor",
        }
    }
}

/// The result of one shadow run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ShadowComparison {
    pub legacy: ShadowOutcome,
    pub unified: ShadowOutcome,
    /// `None` when the two agree.
    pub disagreement: Option<Disagreement>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Disagreement {
    Expected {
        difference: ExpectedDifference,
    },
    /// Not explained by any documented difference: a bug in one of them.
    Unexpected,
}

impl ShadowComparison {
    pub fn agrees(&self) -> bool {
        self.disagreement.is_none()
    }

    pub fn is_unexpected(&self) -> bool {
        matches!(self.disagreement, Some(Disagreement::Unexpected))
    }
}

/// The facts with one documented difference removed, for attribution.
fn without(facts: &[NodeFacts], difference: ExpectedDifference) -> Vec<NodeFacts> {
    facts
        .iter()
        .cloned()
        .map(|mut node| {
            match difference {
                ExpectedDifference::SystemReserved => node.system_reserved = SystemReserved::NONE,
                ExpectedDifference::EnforcedCapacity => {
                    node.resource_policy = NodeResourcePolicy::BestEffort
                }
                ExpectedDifference::InFlightPlacements => node.in_flight = Reservation::default(),
                ExpectedDifference::RoundRobinCursor => {}
            }
            node
        })
        .collect()
}

/// Compare `legacy` with the unified scheduler's answer for `request` over
/// `facts`, and attribute a disagreement. `scheduler` must be fresh for an
/// exact round-robin comparison (its cursor at 0, as a fresh
/// `NodeScheduler`'s is); otherwise a cursor difference is reported as
/// [`ExpectedDifference::RoundRobinCursor`].
pub fn compare_placements(
    legacy: ShadowOutcome,
    config: &SchedulerConfig,
    facts: &[NodeFacts],
    request: &ScheduleRequest<'_>,
    scheduler: &UnifiedScheduler,
) -> ShadowComparison {
    let unified = ShadowOutcome::from_unified(&scheduler.schedule(facts, request));
    if unified == legacy {
        return ShadowComparison {
            legacy,
            unified,
            disagreement: None,
        };
    }
    // Remove the documented differences one at a time, then all together;
    // the first that reproduces the legacy answer is the cause. Each re-run
    // uses a fresh scheduler so the cursor is not what changed.
    let mut remaining: Vec<NodeFacts> = facts.to_vec();
    for difference in [
        ExpectedDifference::SystemReserved,
        ExpectedDifference::EnforcedCapacity,
        ExpectedDifference::InFlightPlacements,
    ] {
        let single = without(facts, difference);
        let alone = UnifiedScheduler::new(config.clone()).schedule(&single, request);
        remaining = without(&remaining, difference);
        let combined = UnifiedScheduler::new(config.clone()).schedule(&remaining, request);
        if ShadowOutcome::from_unified(&alone) == legacy
            || ShadowOutcome::from_unified(&combined) == legacy
        {
            return ShadowComparison {
                legacy,
                unified,
                disagreement: Some(Disagreement::Expected { difference }),
            };
        }
    }
    let round_robin = facts.iter().all(|node| node.load_score().is_none());
    if let (
        true,
        ShadowOutcome::Placed {
            nodes: legacy_nodes,
        },
        ShadowOutcome::Placed {
            nodes: unified_nodes,
        },
    ) = (round_robin, &legacy, &unified)
    {
        let fresh = UnifiedScheduler::new(config.clone()).schedule(&remaining, request);
        if ShadowOutcome::from_unified(&fresh) == legacy
            || (legacy_nodes.len() == unified_nodes.len()
                && same_candidates(legacy_nodes, unified_nodes, facts, request, config))
        {
            return ShadowComparison {
                legacy,
                unified,
                disagreement: Some(Disagreement::Expected {
                    difference: ExpectedDifference::RoundRobinCursor,
                }),
            };
        }
    }
    ShadowComparison {
        legacy,
        unified,
        disagreement: Some(Disagreement::Unexpected),
    }
}

/// Whether every node either side used is one the unified scheduler finds
/// eligible (its single-replica dry run does not exclude it).
fn same_candidates(
    legacy: &[i32],
    unified: &[i32],
    facts: &[NodeFacts],
    request: &ScheduleRequest<'_>,
    config: &SchedulerConfig,
) -> bool {
    let explanation = temps_workload_plane::explain(
        &UnifiedScheduler::new(config.clone()),
        facts,
        request,
        &ExplainContext::default(),
    );
    let eligible: BTreeSet<i32> = explanation
        .recommended
        .iter()
        .map(|node| node.node_id)
        .collect();
    legacy
        .iter()
        .chain(unified)
        .all(|node| eligible.contains(node))
}

// ── The service ─────────────────────────────────────────────────────────────

/// The current scheduling settings of one node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct NodeResourceSettings {
    pub node_id: i32,
    pub resource_policy: NodeResourcePolicy,
    pub system_reserved: SystemReserved,
    /// Whether `system_reserved` is the role's default (the control plane's
    /// when it set none).
    pub system_reserved_is_default: bool,
}

/// A change of one node's scheduling settings; `None` keeps a value.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NodeResourceUpdate {
    pub resource_policy: Option<NodeResourcePolicy>,
    pub system_reserved: Option<SystemReserved>,
}

/// One shadow run's inputs: what the deploy job asks `NodeScheduler`.
#[derive(Debug, Clone)]
pub struct ShadowRequest<'a> {
    pub placement: &'a PlacementSpec,
    pub target_labels: Option<&'a serde_json::Value>,
    pub resources: &'a Resources,
    pub exclude_node_ids: &'a [i32],
    pub image_platforms: &'a [String],
    pub project_slug: Option<&'a str>,
}

/// A pinning recommendation for an environment, with the history it is
/// based on.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EnvironmentPinningRecommendation {
    pub environment_id: i32,
    pub window_days: u32,
    /// Distinct nodes the environment's containers ran on in the window.
    pub nodes_used: Vec<i32>,
    /// Whether the environment is unpinned and moved between nodes: the
    /// case the console shows a recommendation card for.
    pub moved_between_nodes: bool,
    pub recommendation: PinningRecommendation,
}

pub struct WorkloadPlacementService {
    db: Arc<DatabaseConnection>,
    node_service: Arc<NodeService>,
    config_service: Arc<temps_config::ConfigService>,
    scheduler_config: SchedulerConfig,
    socket_grant: DockerSocketGrant,
    heartbeat_threshold_secs: i64,
    /// The isolations the control plane's runtime drivers provide, once the
    /// workload plane is built (ADR-055 Phase 10). Unset means a plain
    /// container host: `process` only.
    control_plane_isolations: std::sync::OnceLock<Vec<Isolation>>,
    /// The control plane's own capacity: it sends no heartbeat, so its
    /// host is sampled in-process (see [`host_control_plane_usage`]).
    control_plane_usage: ControlPlaneUsage,
}

/// Reads the control plane's (node 0) capacity, in the shape a worker's
/// heartbeat reports it.
pub type ControlPlaneUsage = Arc<dyn Fn() -> Option<NodeUsage> + Send + Sync>;

/// The control plane host's capacity from the health loop's sample, sampled
/// now when the loop has not run yet (or ran before core counts were
/// recorded). `None` only when the host reports nothing.
pub fn host_control_plane_usage() -> Option<NodeUsage> {
    let recorded = crate::jobs::node_health_check::latest_control_plane_metrics()
        .map(|(capacity, _)| capacity)
        .filter(|capacity| capacity.get("cpu_cores").is_some());
    let capacity = match recorded {
        Some(capacity) => capacity,
        None => {
            crate::jobs::node_health_check::refresh_control_plane_metrics();
            crate::jobs::node_health_check::latest_control_plane_metrics()?.0
        }
    };
    NodeUsage::from_capacity_json(&capacity)
}

/// The plane's isolation for an environment's setting.
pub fn isolation_for(setting: EnvironmentWorkloadIsolation) -> Isolation {
    match setting {
        EnvironmentWorkloadIsolation::Process => Isolation::Process,
        EnvironmentWorkloadIsolation::MicroVm => Isolation::MicroVm,
    }
}

#[derive(Debug, FromQueryResult)]
struct LoadRow {
    node_id: i32,
    instances: i64,
    cpu_millis: Option<i64>,
    memory_mb: Option<i64>,
}

/// A positive number stored under `key` of the deployment's configuration
/// snapshot (`deployments.deployment_config`, camelCase), else NULL: unset,
/// `0` ("explicitly uncapped") and anything that is not a number all fall
/// through to the next source. `key` is a fixed literal, never input. The
/// column is `json`, hence the `jsonb` cast for `jsonb_typeof`.
fn snapshot_amount_sql(key: &str) -> String {
    format!(
        "(CASE WHEN jsonb_typeof(d.deployment_config::jsonb -> '{key}') = 'number' \
              THEN NULLIF(GREATEST((d.deployment_config::jsonb ->> '{key}')::numeric, 0), 0) END)"
    )
}

/// What the deployment containers on each node reserve, per node: the
/// same request-then-limit-then-default reading the scheduler applies to a
/// deployment in flight (`reservation_of`), from each container's
/// deployment configuration. `$1`/`$2`: the deployment default CPU
/// (millicores) and memory (MB).
fn container_load_sql() -> String {
    // The snapshot stores CPU in microcores (1_000_000 = 1 core) and memory
    // in MB, as the deploy job reads them.
    format!(
        "SELECT COALESCE(dc.node_id, 0) AS node_id, COUNT(*) AS instances, \
                CAST(SUM(COALESCE(CEIL({cpu_request} / 1000), \
                                  CEIL(dc.cpu_limit_cores * 1000), \
                                  CEIL({cpu_limit} / 1000), \
                                  $1)) AS BIGINT) AS cpu_millis, \
                CAST(SUM(COALESCE({memory_request}, {memory_limit}, $2)) AS BIGINT) AS memory_mb \
           FROM deployment_containers dc \
           LEFT JOIN deployments d ON d.id = dc.deployment_id \
          WHERE dc.deleted_at IS NULL \
            AND (dc.status IS NULL OR dc.status NOT IN ('exited', 'stopped', 'failed', 'cancelled')) \
          GROUP BY 1",
        cpu_request = snapshot_amount_sql("cpuRequest"),
        cpu_limit = snapshot_amount_sql("cpuLimit"),
        memory_request = snapshot_amount_sql("memoryRequest"),
        memory_limit = snapshot_amount_sql("memoryLimit"),
    )
}

#[derive(Debug, FromQueryResult)]
struct NodeCountRow {
    node_id: i32,
    containers: i64,
}

#[derive(Debug, FromQueryResult)]
struct LinkedServiceRow {
    node_id: i32,
    name: String,
}

impl WorkloadPlacementService {
    /// `legacy` supplies the settings both schedulers share (threshold,
    /// control-plane policy and platform, heartbeat window, socket grant),
    /// so the comparison differs only where ADR-055 says it does.
    pub fn new(
        db: Arc<DatabaseConnection>,
        node_service: Arc<NodeService>,
        config_service: Arc<temps_config::ConfigService>,
        legacy: &NodeScheduler,
    ) -> Self {
        Self {
            db,
            node_service,
            config_service,
            scheduler_config: legacy.unified_scheduler_config(),
            socket_grant: legacy.docker_socket_grant().clone(),
            heartbeat_threshold_secs: legacy.heartbeat_threshold_secs(),
            control_plane_isolations: std::sync::OnceLock::new(),
            control_plane_usage: Arc::new(host_control_plane_usage),
        }
    }

    /// Read the control plane's capacity from `usage` instead of this host.
    pub fn with_control_plane_usage(mut self, usage: ControlPlaneUsage) -> Self {
        self.control_plane_usage = usage;
        self
    }

    /// Record what the control plane's drivers provide, so the explain
    /// endpoint answers as the plane schedules. Set once, when the plane is
    /// built; a second call is ignored.
    pub fn set_control_plane_isolations(&self, isolations: Vec<Isolation>) {
        if self.control_plane_isolations.set(isolations).is_err() {
            tracing::debug!("control-plane isolations already recorded; keeping the first");
        }
    }

    pub fn scheduler_config(&self) -> &SchedulerConfig {
        &self.scheduler_config
    }

    /// The ADR-045 gate for `project_slug`, when this control plane declares
    /// the project.
    pub fn socket_gate_for(&self, project_slug: Option<&str>) -> Option<SocketGate> {
        let slug = project_slug?;
        self.socket_grant.declares(slug).then(|| SocketGate {
            project_slug: slug.to_string(),
            control_plane_grants: self.socket_grant.allows(slug),
        })
    }

    async fn control_plane_settings(
        &self,
    ) -> Result<temps_core::ControlPlaneResourceSettings, WorkloadPlacementError> {
        self.config_service
            .get_settings()
            .await
            .map(|settings| settings.control_plane_resources)
            .map_err(|e| WorkloadPlacementError::Settings {
                operation: "read",
                reason: e.to_string(),
            })
    }

    fn control_plane_reserved(
        settings: &temps_core::ControlPlaneResourceSettings,
    ) -> SystemReserved {
        // The §5 default for a control plane assumes its database is local
        // (the larger reserve): the conservative reading, since Temps cannot
        // tell where the operator runs PostgreSQL.
        let default = SystemReserved::CONTROL_PLANE_LOCAL_DATABASE;
        SystemReserved {
            cpu_millis: settings
                .system_reserved_cpu_millis
                .unwrap_or(default.cpu_millis),
            memory_mb: settings
                .system_reserved_memory_mb
                .unwrap_or(default.memory_mb),
        }
    }

    /// What the deployment containers on each node hold: each container's
    /// configured CPU and memory -- its deployment's request, else its
    /// limit, else the deployment default -- exactly as the scheduler
    /// reserved them while the deployment was in flight. Without this a
    /// 2 GiB deployment would shrink to the 512 MiB default the moment it
    /// completed and free room that is not there. One grouped query joined
    /// to the deployments, never one per container.
    async fn container_load(&self) -> Result<BTreeMap<i32, ContainerLoad>, WorkloadPlacementError> {
        let default =
            temps_workload_plane::scheduler::kind_default_reservation(WorkloadKind::Deployment);
        let rows = LoadRow::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            container_load_sql(),
            vec![
                (default.cpu_millis as i64).into(),
                (default.memory_mb as i64).into(),
            ],
        ))
        .all(self.db.as_ref())
        .await
        .map_err(db_error("summing the containers running on each node"))?;
        Ok(rows
            .into_iter()
            .map(|row| {
                let instances = u32::try_from(row.instances).unwrap_or(u32::MAX);
                (
                    row.node_id,
                    ContainerLoad {
                        reservation: Reservation {
                            cpu_millis: row.cpu_millis.unwrap_or(0).max(0) as u64,
                            memory_mb: row.memory_mb.unwrap_or(0).max(0) as u64,
                        },
                        instances,
                    },
                )
            })
            .collect())
    }

    /// Every node as the scheduler sees it: the control plane first (when
    /// this process may run workloads), then workers by name, as
    /// `NodeScheduler` orders them.
    pub async fn node_facts(
        &self,
        project_slug: Option<&str>,
    ) -> Result<Vec<NodeFacts>, WorkloadPlacementError> {
        let rows = self
            .node_service
            .list_all()
            .await
            .map_err(node_error("listing nodes"))?;
        let settings: BTreeMap<i32, node_resource_settings::Model> =
            node_resource_settings::Entity::find()
                .all(self.db.as_ref())
                .await
                .map_err(db_error("reading node resource settings"))?
                .into_iter()
                .map(|row| (row.id, row))
                .collect();
        let load = self.container_load().await?;
        let granting: BTreeSet<i32> = match self.socket_gate_for(project_slug) {
            Some(gate) => self
                .node_service
                .granting_node_ids_and_names(&gate.project_slug)
                .await
                .map_err(node_error("reading which nodes grant the Docker socket"))?
                .into_iter()
                .map(|(id, _)| id)
                .collect(),
            None => BTreeSet::new(),
        };
        let control = self.control_plane_settings().await?;
        let now = Utc::now();
        let mut facts = Vec::with_capacity(rows.len() + 1);
        let mut control_plane =
            NodeFacts::control_plane(self.scheduler_config.local_platform.clone());
        control_plane.resource_policy = if control.enforce_reservations {
            NodeResourcePolicy::Enforced
        } else {
            NodeResourcePolicy::BestEffort
        };
        control_plane.system_reserved = Self::control_plane_reserved(&control);
        // Its capacity: without it an enforced node 0 could place nothing
        // that reserves CPU or memory (`CapacityUnknown`). Only the capacity,
        // not the live load, so node 0 is never dropped by the load threshold
        // (as it never was before it reported anything).
        control_plane.usage = (self.control_plane_usage)()
            .map(|usage| NodeUsage {
                cpu_percent: None,
                memory_used_bytes: None,
                ..usage
            })
            .filter(|usage| usage.cpu_cores.is_some() || usage.memory_total_bytes.is_some());
        if let Some(isolations) = self.control_plane_isolations.get() {
            control_plane.isolations = isolations.clone();
        }
        if let Some(local) = load.get(&CONTROL_PLANE_NODE_ID) {
            control_plane.committed = local.reservation;
            control_plane.live_instances = local.instances;
        }
        facts.push(control_plane);
        facts.extend(rows.iter().map(|node| {
            node_facts_from_row(
                node,
                settings.get(&node.id),
                load.get(&node.id),
                granting.contains(&node.id),
                now,
                self.heartbeat_threshold_secs,
            )
        }));
        Ok(facts)
    }

    /// The deploy path's assignment for each placed node (`0` = local).
    /// One query, whatever the replica count.
    pub async fn assignments_for(
        &self,
        node_ids: &[i32],
    ) -> Result<Vec<NodeAssignment>, WorkloadPlacementError> {
        let rows: BTreeMap<i32, nodes::Model> =
            if node_ids.iter().any(|id| *id != CONTROL_PLANE_NODE_ID) {
                self.node_service
                    .list_all()
                    .await
                    .map_err(node_error("reading the placed nodes"))?
                    .into_iter()
                    .map(|node| (node.id, node))
                    .collect()
            } else {
                BTreeMap::new()
            };
        node_ids
            .iter()
            .map(|node_id| {
                if *node_id == CONTROL_PLANE_NODE_ID {
                    return Ok(NodeAssignment::Local);
                }
                let node = rows
                    .get(node_id)
                    .ok_or(WorkloadPlacementError::NodeNotFound { node_id: *node_id })?;
                Ok(NodeAssignment::Remote {
                    node_id: node.id,
                    node_name: node.name.clone(),
                    address: node.address.clone(),
                    private_address: node.data_address().to_string(),
                    platform: node.architecture.clone(),
                })
            })
            .collect()
    }

    /// Run `NodeScheduler` and the unified scheduler on the same request and
    /// compare them. `legacy` is the caller's result when it already has one
    /// (the deploy job's own placement), else `NodeScheduler` is asked.
    pub async fn shadow_run(
        &self,
        legacy_scheduler: &NodeScheduler,
        legacy: Option<ShadowOutcome>,
        request: &ShadowRequest<'_>,
    ) -> Result<ShadowComparison, WorkloadPlacementError> {
        let legacy = match legacy {
            Some(outcome) => outcome,
            None => ShadowOutcome::from_legacy(
                &legacy_scheduler
                    .schedule_placement(ReplicaPlacementRequest {
                        replica_count: request.placement.replicas,
                        labels: request.target_labels,
                        target_node_ids: request.placement.nodes.as_deref(),
                        anti_affinity: request.placement.spread,
                        exclude_node_ids: request.exclude_node_ids,
                        image_platforms: request.image_platforms,
                        project_slug: request.project_slug,
                        exclude_control_plane: false,
                    })
                    .await,
            ),
        };
        let facts = self.node_facts(request.project_slug).await?;
        let schedule_request = ScheduleRequest {
            kind: WorkloadKind::Deployment,
            placement: request.placement,
            resources: request.resources,
            isolation: Isolation::Process,
            image_platforms: request.image_platforms,
            socket_gate: self.socket_gate_for(request.project_slug),
            exclude_node_ids: request.exclude_node_ids,
            exclude_control_plane: false,
            affinity_nodes: &[],
        };
        let scheduler = UnifiedScheduler::new(self.scheduler_config.clone());
        Ok(compare_placements(
            legacy,
            &self.scheduler_config,
            &facts,
            &schedule_request,
            &scheduler,
        ))
    }

    async fn environment(
        &self,
        environment_id: i32,
    ) -> Result<environments::Model, WorkloadPlacementError> {
        environments::Entity::find_by_id(environment_id)
            .one(self.db.as_ref())
            .await
            .map_err(db_error("reading the environment"))?
            .filter(|environment| environment.deleted_at.is_none())
            .ok_or(WorkloadPlacementError::EnvironmentNotFound { environment_id })
    }

    async fn environment_and_project(
        &self,
        environment_id: i32,
    ) -> Result<(environments::Model, projects::Model), WorkloadPlacementError> {
        let environment = self.environment(environment_id).await?;
        let project = projects::Entity::find_by_id(environment.project_id)
            .one(self.db.as_ref())
            .await
            .map_err(db_error("reading the environment's project"))?
            .ok_or(WorkloadPlacementError::ProjectNotFound {
                project_id: environment.project_id,
                environment_id,
            })?;
        Ok((environment, project))
    }

    pub async fn environment_workload_mode(
        &self,
        environment_id: i32,
    ) -> Result<EnvironmentWorkloadMode, WorkloadPlacementError> {
        Ok(self.environment(environment_id).await?.workload_mode)
    }

    /// Set the environment's mode; returns the previous one. Moving back to
    /// `legacy` is the Phase 5 rollback.
    pub async fn set_environment_workload_mode(
        &self,
        environment_id: i32,
        mode: EnvironmentWorkloadMode,
    ) -> Result<EnvironmentWorkloadMode, WorkloadPlacementError> {
        let environment = self.environment(environment_id).await?;
        let previous = environment.workload_mode;
        if previous != mode {
            let mut active: environments::ActiveModel = environment.into();
            active.workload_mode = Set(mode);
            active.updated_at = Set(Utc::now());
            active
                .update(self.db.as_ref())
                .await
                .map_err(db_error("writing the environment's workload mode"))?;
        }
        Ok(previous)
    }

    async fn environment_isolation_row(
        &self,
        environment_id: i32,
    ) -> Result<environment_workload_isolation::Model, WorkloadPlacementError> {
        environment_workload_isolation::Entity::find_by_id(environment_id)
            .one(self.db.as_ref())
            .await
            .map_err(db_error("reading the environment's workload isolation"))?
            .filter(|environment| environment.deleted_at.is_none())
            .ok_or(WorkloadPlacementError::EnvironmentNotFound { environment_id })
    }

    /// The isolation the environment's plane-run deployments request.
    pub async fn environment_workload_isolation(
        &self,
        environment_id: i32,
    ) -> Result<EnvironmentWorkloadIsolation, WorkloadPlacementError> {
        Ok(self
            .environment_isolation_row(environment_id)
            .await?
            .workload_isolation)
    }

    /// Set the environment's isolation; returns the previous one. Only
    /// `enforce` deployments honour it; `process` is the rollback.
    pub async fn set_environment_workload_isolation(
        &self,
        environment_id: i32,
        isolation: EnvironmentWorkloadIsolation,
    ) -> Result<EnvironmentWorkloadIsolation, WorkloadPlacementError> {
        let row = self.environment_isolation_row(environment_id).await?;
        let previous = row.workload_isolation;
        if previous != isolation {
            let mut active: environment_workload_isolation::ActiveModel = row.into();
            active.workload_isolation = Set(isolation);
            active.updated_at = Set(Utc::now());
            active
                .update(self.db.as_ref())
                .await
                .map_err(db_error("writing the environment's workload isolation"))?;
        }
        Ok(previous)
    }

    pub async fn node_resources(
        &self,
        node_id: i32,
    ) -> Result<NodeResourceSettings, WorkloadPlacementError> {
        if node_id == CONTROL_PLANE_NODE_ID {
            let control = self.control_plane_settings().await?;
            return Ok(NodeResourceSettings {
                node_id,
                resource_policy: if control.enforce_reservations {
                    NodeResourcePolicy::Enforced
                } else {
                    NodeResourcePolicy::BestEffort
                },
                system_reserved: Self::control_plane_reserved(&control),
                system_reserved_is_default: control.system_reserved_cpu_millis.is_none()
                    && control.system_reserved_memory_mb.is_none(),
            });
        }
        let row = node_resource_settings::Entity::find_by_id(node_id)
            .one(self.db.as_ref())
            .await
            .map_err(db_error("reading the node's resource settings"))?
            .ok_or(WorkloadPlacementError::NodeNotFound { node_id })?;
        Ok(NodeResourceSettings {
            node_id,
            resource_policy: row.resource_policy,
            system_reserved: row.system_reserved,
            system_reserved_is_default: row.system_reserved
                == SystemReserved::default_for_role(&row.role),
        })
    }

    fn validate_reserved(reserved: &SystemReserved) -> Result<(), WorkloadPlacementError> {
        if reserved.cpu_millis > MAX_RESERVED_CPU_MILLIS {
            return Err(WorkloadPlacementError::Validation {
                message: format!(
                    "system_reserved.cpu_millis {} exceeds the maximum of {MAX_RESERVED_CPU_MILLIS}",
                    reserved.cpu_millis
                ),
            });
        }
        if reserved.memory_mb > MAX_RESERVED_MEMORY_MB {
            return Err(WorkloadPlacementError::Validation {
                message: format!(
                    "system_reserved.memory_mb {} exceeds the maximum of {MAX_RESERVED_MEMORY_MB}",
                    reserved.memory_mb
                ),
            });
        }
        Ok(())
    }

    /// Change a node's scheduling settings (node 0: the control plane's,
    /// stored in the instance settings). Returns `(previous, current)`.
    pub async fn set_node_resources(
        &self,
        node_id: i32,
        update: NodeResourceUpdate,
    ) -> Result<(NodeResourceSettings, NodeResourceSettings), WorkloadPlacementError> {
        if let Some(reserved) = &update.system_reserved {
            Self::validate_reserved(reserved)?;
        }
        let previous = self.node_resources(node_id).await?;
        if node_id == CONTROL_PLANE_NODE_ID
            && update.resource_policy == Some(NodeResourcePolicy::Enforced)
        {
            let usage = (self.control_plane_usage)();
            let missing: Vec<&str> = [
                ("CPU core count", usage.and_then(|u| u.cpu_cores).is_none()),
                (
                    "total memory",
                    usage.and_then(|u| u.memory_total_bytes).is_none(),
                ),
            ]
            .into_iter()
            .filter_map(|(what, missing)| missing.then_some(what))
            .collect();
            if !missing.is_empty() {
                return Err(WorkloadPlacementError::Validation {
                    message: format!(
                        "Cannot enforce reservations on the control plane (node 0): its host \
                         does not report its {}, so every reserving workload would be \
                         unschedulable. Keep node 0 on best_effort",
                        missing.join(" or ")
                    ),
                });
            }
        }
        if node_id == CONTROL_PLANE_NODE_ID {
            let mut settings = self.config_service.get_settings().await.map_err(|e| {
                WorkloadPlacementError::Settings {
                    operation: "read",
                    reason: e.to_string(),
                }
            })?;
            if let Some(policy) = update.resource_policy {
                settings.control_plane_resources.enforce_reservations =
                    policy == NodeResourcePolicy::Enforced;
            }
            if let Some(reserved) = update.system_reserved {
                settings.control_plane_resources.system_reserved_cpu_millis =
                    Some(reserved.cpu_millis);
                settings.control_plane_resources.system_reserved_memory_mb =
                    Some(reserved.memory_mb);
            }
            self.config_service
                .update_settings(settings)
                .await
                .map_err(|e| WorkloadPlacementError::Settings {
                    operation: "written",
                    reason: e.to_string(),
                })?;
        } else {
            let mut active = node_resource_settings::ActiveModel {
                id: Set(node_id),
                ..Default::default()
            };
            if let Some(policy) = update.resource_policy {
                active.resource_policy = Set(policy);
            }
            if let Some(reserved) = update.system_reserved {
                active.system_reserved = Set(reserved);
            }
            active.updated_at = Set(Utc::now());
            active
                .update(self.db.as_ref())
                .await
                .map_err(db_error("writing the node's resource settings"))?;
        }
        let current = self.node_resources(node_id).await?;
        Ok((previous, current))
    }

    /// The nodes hosting services the project is linked to (its databases).
    async fn linked_service_nodes(
        &self,
        project_id: i32,
    ) -> Result<BTreeMap<i32, Vec<String>>, WorkloadPlacementError> {
        let rows = LinkedServiceRow::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT COALESCE(es.node_id, 0) AS node_id, es.name AS name \
               FROM project_services ps \
               JOIN external_services es ON es.id = ps.service_id \
              WHERE ps.project_id = $1 \
              ORDER BY es.name",
            vec![project_id.into()],
        ))
        .all(self.db.as_ref())
        .await
        .map_err(db_error("reading the project's linked services"))?;
        let mut by_node: BTreeMap<i32, Vec<String>> = BTreeMap::new();
        for row in rows {
            by_node.entry(row.node_id).or_default().push(row.name);
        }
        Ok(by_node)
    }

    /// Nodes that hold the environment's current image (it runs there).
    async fn image_cached_on(
        &self,
        environment: &environments::Model,
    ) -> Result<BTreeSet<i32>, WorkloadPlacementError> {
        let Some(deployment_id) = environment.current_deployment_id else {
            return Ok(BTreeSet::new());
        };
        let rows = NodeCountRow::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT COALESCE(node_id, 0) AS node_id, COUNT(*) AS containers \
               FROM deployment_containers \
              WHERE deployment_id = $1 AND deleted_at IS NULL AND image_name IS NOT NULL \
              GROUP BY 1",
            vec![deployment_id.into()],
        ))
        .all(self.db.as_ref())
        .await
        .map_err(db_error("reading where the current image runs"))?;
        Ok(rows.into_iter().map(|row| row.node_id).collect())
    }

    /// How many of the environment's containers each node ran since `since`.
    async fn placement_history(
        &self,
        environment_id: i32,
        since: DateTime<Utc>,
    ) -> Result<BTreeMap<i32, u64>, WorkloadPlacementError> {
        let rows = NodeCountRow::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT COALESCE(dc.node_id, 0) AS node_id, COUNT(*) AS containers \
               FROM deployment_containers dc \
               JOIN deployments d ON d.id = dc.deployment_id \
              WHERE d.environment_id = $1 AND dc.created_at >= $2 \
              GROUP BY 1",
            vec![environment_id.into(), since.into()],
        ))
        .all(self.db.as_ref())
        .await
        .map_err(db_error("reading where the environment ran"))?;
        Ok(rows
            .into_iter()
            .map(|row| (row.node_id, row.containers.max(0) as u64))
            .collect())
    }

    fn replicas_of(environment: &environments::Model, project: &projects::Model) -> u32 {
        environment
            .deployment_config
            .as_ref()
            .map(|c| c.replicas)
            .or_else(|| project.deployment_config.as_ref().map(|c| c.replicas))
            .map(|replicas| replicas.max(1) as u32)
            .unwrap_or(1)
    }

    /// A scheduler dry run for the environment's next deployment.
    pub async fn explain_environment(
        &self,
        environment_id: i32,
    ) -> Result<(Explanation, ExplainContext), WorkloadPlacementError> {
        let (environment, project) = self.environment_and_project(environment_id).await?;
        // Only `enforce` honours the isolation setting, so only it is
        // explained with it.
        let isolation = if environment.workload_mode == EnvironmentWorkloadMode::Enforce {
            isolation_for(self.environment_workload_isolation(environment_id).await?)
        } else {
            Isolation::Process
        };
        let env_cfg = environment.deployment_config.as_ref();
        let proj_cfg = project.deployment_config.as_ref();
        let mut placement =
            placement_spec_for(env_cfg, proj_cfg, Self::replicas_of(&environment, &project))?;
        // A node-local volume pins the environment (ADR-055 §8, §12); the
        // explanation shows the pin and, if its node is ineligible, why the
        // replica would stay unscheduled.
        if let Some(pin) = self.volume_pin(environment_id).await? {
            placement.nodes = Some(vec![pin.node_id]);
        }
        let resources = resources_for(env_cfg, proj_cfg);
        let context = ExplainContext {
            linked_service_nodes: self.linked_service_nodes(project.id).await?,
            image_cached_on: self.image_cached_on(&environment).await?,
        };
        let facts = self.node_facts(Some(&project.slug)).await?;
        let request = ScheduleRequest {
            kind: WorkloadKind::Deployment,
            placement: &placement,
            resources: &resources,
            isolation,
            image_platforms: &[],
            socket_gate: self.socket_gate_for(Some(&project.slug)),
            exclude_node_ids: &[],
            exclude_control_plane: false,
            affinity_nodes: &[],
        };
        let explanation = temps_workload_plane::explain(
            &UnifiedScheduler::new(self.scheduler_config.clone()),
            &facts,
            &request,
            &context,
        );
        Ok((explanation, context))
    }

    /// Where the environment's attached volumes pin its replicas, if any
    /// has a node yet ("pinned to node 0 by volume `data`").
    pub async fn volume_pin(
        &self,
        environment_id: i32,
    ) -> Result<Option<temps_workload_volumes::VolumePin>, WorkloadPlacementError> {
        let records = temps_workload_plane::VolumeStore::new(self.db.clone())
            .all_of(&crate::services::workload_volumes::environment_volume_owner(environment_id))
            .await
            .map_err(|e| WorkloadPlacementError::Settings {
                operation: "read for the environment's volumes",
                reason: e.to_string(),
            })?;
        let mounts: Vec<temps_workload::VolumeMount> = records
            .iter()
            .filter_map(temps_workload_plane::VolumeRecord::mount)
            .collect();
        if mounts.is_empty() {
            return Ok(None);
        }
        temps_workload_plane::volumes::pin_for(
            temps_workload::WorkloadRef::new(WorkloadKind::Deployment, 0),
            &mounts,
            &records,
            None,
        )
        .map_err(|e| WorkloadPlacementError::Validation {
            message: format!("the volumes of environment {environment_id}: {e}"),
        })
    }

    /// A recommendation for an environment that moved between nodes
    /// (ADR-055 §8: recommended, never imposed).
    pub async fn pinning_recommendation(
        &self,
        environment_id: i32,
        window_days: u32,
    ) -> Result<EnvironmentPinningRecommendation, WorkloadPlacementError> {
        if window_days == 0 || window_days > 90 {
            return Err(WorkloadPlacementError::Validation {
                message: format!("window_days must be between 1 and 90, got {window_days}"),
            });
        }
        let (environment, project) = self.environment_and_project(environment_id).await?;
        let (explanation, context) = self.explain_environment(environment_id).await?;
        let since = Utc::now() - chrono::Duration::days(i64::from(window_days));
        let history = self.placement_history(environment_id, since).await?;
        let placement = placement_spec_for(
            environment.deployment_config.as_ref(),
            project.deployment_config.as_ref(),
            Self::replicas_of(&environment, &project),
        )?;
        let current = placement.nodes.clone().unwrap_or_default();
        let mut recommendation = temps_workload_plane::recommend_pinning(
            &explanation,
            &history,
            &context,
            placement.replicas,
            &current,
        );
        let nodes_used: Vec<i32> = history.keys().copied().collect();
        let moved_between_nodes = current.is_empty() && nodes_used.len() > 1;
        if moved_between_nodes {
            let linked = context
                .linked_service_nodes
                .iter()
                .map(|(node, services)| format!("{} on node {node}", services.join(", ")))
                .collect::<Vec<_>>();
            recommendation.reasons.insert(
                0,
                format!(
                    "'{}' moved between {} nodes in {window_days} days{}",
                    environment.name,
                    nodes_used.len(),
                    if linked.is_empty() {
                        String::new()
                    } else {
                        format!("; its linked services run {}", linked.join(", "))
                    }
                ),
            );
        }
        Ok(EnvironmentPinningRecommendation {
            environment_id,
            window_days,
            nodes_used,
            moved_between_nodes,
            recommendation,
        })
    }
}

#[async_trait]
impl NodeInventory for WorkloadPlacementService {
    async fn nodes(&self, project_slug: Option<&str>) -> Result<Vec<NodeFacts>, String> {
        self.node_facts(project_slug)
            .await
            .map_err(|error| error.to_string())
    }

    fn socket_gate(&self, project_slug: Option<&str>) -> Option<SocketGate> {
        self.socket_gate_for(project_slug)
    }

    async fn explain_context(
        &self,
        _hints: &PlacementHints,
        _image: Option<&str>,
    ) -> Result<ExplainContext, String> {
        Ok(ExplainContext::default())
    }
}

/// Log a shadow comparison for the deploy log and tracing. Returns the line
/// to write to the deploy log.
pub fn describe_shadow(comparison: &ShadowComparison) -> String {
    let side = |outcome: &ShadowOutcome| match outcome {
        ShadowOutcome::Placed { nodes } => format!("nodes {nodes:?}"),
        ShadowOutcome::Refused { reason } => format!("refused ({reason})"),
    };
    match &comparison.disagreement {
        None => format!(
            "Workload plane shadow run: the unified scheduler agrees ({})",
            side(&comparison.unified)
        ),
        Some(Disagreement::Expected { difference }) => format!(
            "Workload plane shadow run: the unified scheduler would use {} instead of {} (expected \
             difference: {})",
            side(&comparison.unified),
            side(&comparison.legacy),
            difference.as_str()
        ),
        Some(Disagreement::Unexpected) => format!(
            "WARNING: workload plane shadow run disagrees: the unified scheduler would use {}, \
             NodeScheduler chose {}; this is not one of the documented differences",
            side(&comparison.unified),
            side(&comparison.legacy)
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with(
        target_nodes: Option<Vec<i32>>,
        target_labels: Option<serde_json::Value>,
        anti_affinity: bool,
    ) -> DeploymentConfig {
        DeploymentConfig {
            target_nodes,
            target_labels,
            anti_affinity,
            ..Default::default()
        }
    }

    #[test]
    fn the_environment_overrides_the_project_pin_by_pin() {
        let project = config_with(
            Some(vec![3]),
            Some(serde_json::json!({"region": ["us", "eu"], "gpu": true})),
            true,
        );
        // The environment sets only its own pins; labels inherit.
        let environment = config_with(Some(vec![5, 6]), None, false);
        let placement = placement_spec_for(Some(&environment), Some(&project), 2).unwrap();
        assert_eq!(placement.nodes, Some(vec![5, 6]));
        assert!(!placement.spread, "the environment's anti_affinity wins");
        let labels = placement.labels.expect("inherited labels");
        assert_eq!(
            labels.get("gpu"),
            Some(&LabelValues::One("true".to_string())),
            "a bool selector compares as its string, as NodeScheduler does"
        );
        assert_eq!(
            labels.get("region"),
            Some(&LabelValues::AnyOf(vec!["us".into(), "eu".into()]))
        );
        assert_eq!(placement.replicas, 2);

        // Empty values are the API's "clear the override" sentinel.
        let cleared = config_with(Some(vec![]), Some(serde_json::json!({})), true);
        let placement = placement_spec_for(Some(&cleared), Some(&project), 0).unwrap();
        assert_eq!(placement.nodes, Some(vec![3]), "an empty list inherits");
        assert_eq!(placement.replicas, 1);
        let none = placement_spec_for(None, None, 1).unwrap();
        assert_eq!(none.nodes, None);
        assert!(none.spread, "anti-affinity defaults on");
    }

    #[test]
    fn a_malformed_selector_is_a_validation_error_naming_it() {
        let error = label_selector_from_json(Some(&serde_json::json!(["gpu"]))).unwrap_err();
        assert!(
            matches!(&error, WorkloadPlacementError::Validation { message } if message.contains("[\"gpu\"]")),
            "{error}"
        );
    }

    #[test]
    fn current_limits_carry_over_as_limits() {
        let project = DeploymentConfig {
            cpu_limit: Some(1_500_000),
            memory_limit: Some(512),
            cpu_request: Some(250_000),
            ..Default::default()
        };
        // Some(0) is "explicitly uncapped" and stops inheriting the cap.
        let environment = DeploymentConfig {
            memory_limit: Some(0),
            ..Default::default()
        };
        let resources = resources_for(Some(&environment), Some(&project));
        let limits = resources.limits.expect("limits");
        assert_eq!(limits.cpu_millis, Some(1500));
        assert_eq!(limits.memory_mb, None, "uncapped is no memory limit");
        assert_eq!(resources.requests.and_then(|r| r.cpu_millis), Some(250));
        assert_eq!(resources_for(None, None), Resources::default());
        // A sub-millicore value is rounded up, never loosened to zero.
        assert_eq!(micro_to_millis(10_001), Some(11));
    }

    fn node_row(id: i32, name: &str, role: &str) -> nodes::Model {
        let now = Utc::now();
        nodes::Model {
            architecture: Some("linux/amd64".to_string()),
            id,
            name: name.to_string(),
            token_hash: String::new(),
            token_encrypted: None,
            address: format!("https://10.231.0.{id}:3100"),
            private_address: format!("10.231.0.{id}"),
            public_endpoint: None,
            wg_public_key: None,
            role: role.to_string(),
            status: "active".to_string(),
            labels: serde_json::json!({"temps.sh/role": "dedicated", "tier": 2}),
            capacity: serde_json::json!({"cpu_percent": 10.0, "cpu_cores": 4}),
            last_heartbeat: Some(now - chrono::Duration::seconds(300)),
            edge_public_key: None,
            compute_cidr: None,
            underlay_address: None,
            mesh_wg_public_key: None,
            mesh_wg_endpoint: None,
            mesh_wg_address: None,
            dns_resolver_running: None,
            dns_resolver_tasks_alive: None,
            dns_resolver_last_sync_at: None,
            dns_resolver_consecutive_failures: 0,
            dns_resolver_last_error: None,
            dns_resolver_record_count: None,
            deployments_without_resolver: None,
            failover_at: None,
            build_limits: None,
            public_ingress_enabled: false,
            public_ingress_running: None,
            public_ingress_last_error: None,
            public_ingress_certificate_count: None,
            public_ingress_route_count: None,
            public_ingress_unsupported_route_count: None,
            public_ingress_unsupported_reasons: serde_json::json!([]),
            occupied_ranges: None,
            overlay_status: None,
            overlay_last_error: None,
            overlay_ready_cidr: None,
            overlay_remediation: None,
            overlay_network_name: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn a_node_row_maps_to_facts_with_its_role_reserve_and_load() {
        let row = node_row(4, "worker-a", "worker");
        let load = ContainerLoad {
            reservation: Reservation {
                cpu_millis: 500,
                memory_mb: 1024,
            },
            instances: 2,
        };
        let facts = node_facts_from_row(&row, None, Some(&load), false, Utc::now(), 90);
        assert!(!facts.heartbeat_fresh, "a 300s-old heartbeat is stale");
        assert_eq!(facts.role, NodeRole::Dedicated);
        assert_eq!(facts.labels.get("tier").map(String::as_str), Some("2"));
        assert_eq!(
            facts.system_reserved,
            SystemReserved::WORKER,
            "the role default"
        );
        assert_eq!(facts.resource_policy, NodeResourcePolicy::BestEffort);
        assert_eq!(facts.committed, load.reservation);
        assert_eq!(facts.live_instances, 2);
        assert_eq!(facts.usage.and_then(|u| u.cpu_cores), Some(4.0));

        let settings = node_resource_settings::Model {
            id: 4,
            name: "worker-a".into(),
            role: "worker".into(),
            resource_policy: NodeResourcePolicy::Enforced,
            system_reserved: SystemReserved {
                cpu_millis: 1000,
                memory_mb: 2048,
            },
            updated_at: Utc::now(),
        };
        let facts = node_facts_from_row(&row, Some(&settings), None, true, Utc::now(), 600);
        assert!(facts.heartbeat_fresh);
        assert!(facts.grants_socket);
        assert_eq!(facts.resource_policy, NodeResourcePolicy::Enforced);
        assert_eq!(facts.system_reserved.memory_mb, 2048);
    }

    #[tokio::test]
    async fn an_enforced_control_plane_with_room_schedules_a_deployment() {
        let test_db = match temps_database::test_utils::TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(e) => {
                println!("Postgres unavailable, skipping the enforced node 0 test: {e}");
                return;
            }
        };
        let db = test_db.connection_arc();
        let node_service = Arc::new(NodeService::new(db.clone()));
        let legacy = NodeScheduler::new(node_service.clone());
        let unknown = WorkloadPlacementService::new(
            db.clone(),
            node_service.clone(),
            proof_config_service(db.clone()),
            &legacy,
        )
        .with_control_plane_usage(Arc::new(|| None));
        let enforce = NodeResourceUpdate {
            resource_policy: Some(NodeResourcePolicy::Enforced),
            system_reserved: None,
        };
        let refused = unknown
            .set_node_resources(CONTROL_PLANE_NODE_ID, enforce)
            .await
            .expect_err("no capacity, no enforcement");
        assert!(
            matches!(&refused, WorkloadPlacementError::Validation { message }
                if message.contains("CPU core count") && message.contains("node 0")),
            "{refused}"
        );

        // A 4-core, 16 GB single-node install with nothing running.
        let service = WorkloadPlacementService::new(
            db.clone(),
            node_service,
            proof_config_service(db.clone()),
            &legacy,
        )
        .with_control_plane_usage(Arc::new(|| {
            Some(NodeUsage {
                cpu_percent: Some(10.0),
                memory_used_bytes: Some(2.0 * 1024.0 * 1024.0 * 1024.0),
                memory_total_bytes: Some(16.0 * 1024.0 * 1024.0 * 1024.0),
                cpu_cores: Some(4.0),
            })
        }));
        service
            .set_node_resources(CONTROL_PLANE_NODE_ID, enforce)
            .await
            .expect("enforced");
        let nodes = service.node_facts(None).await.expect("facts");
        let control_plane = &nodes[0];
        assert_eq!(control_plane.id, CONTROL_PLANE_NODE_ID);
        assert_eq!(control_plane.resource_policy, NodeResourcePolicy::Enforced);
        assert_eq!(control_plane.usage.and_then(|u| u.cpu_cores), Some(4.0));

        let placement_spec = temps_workload::PlacementSpec::default();
        let resources = temps_workload::Resources::default();
        let placement = temps_workload_plane::scheduler::UnifiedScheduler::new(
            service.scheduler_config().clone(),
        )
        .schedule(
            &nodes,
            &temps_workload_plane::scheduler::ScheduleRequest {
                kind: temps_workload::WorkloadKind::Deployment,
                placement: &placement_spec,
                resources: &resources,
                isolation: Isolation::Process,
                image_platforms: &[],
                socket_gate: None,
                exclude_node_ids: &[],
                exclude_control_plane: false,
                affinity_nodes: &[],
            },
        )
        .expect("an ordinary deployment fits an enforced node 0 with room");
        assert_eq!(placement.nodes, vec![CONTROL_PLANE_NODE_ID]);
    }

    /// A completed deployment keeps the memory (and CPU) it was admitted
    /// with: on a 4 GiB node 0 with a 512 MiB reserve, one running 2 GiB
    /// deployment leaves 1.5 GiB, so a second 2 GiB one is refused. Counting
    /// the finished replica at the 512 MiB default would admit it.
    #[tokio::test]
    async fn committed_load_keeps_each_deployments_configured_reservation() {
        use temps_entities::deployment_config::DeploymentConfigSnapshot;
        use temps_entities::upstream_config::UpstreamList;
        use temps_entities::{deployment_containers, deployments};
        let test_db = match temps_database::test_utils::TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(e) => {
                println!("Postgres unavailable, skipping the committed load test: {e}");
                return;
            }
        };
        let db = test_db.connection_arc();
        let project = projects::ActiveModel {
            name: Set("big-app".to_string()),
            slug: Set("big-app".to_string()),
            repo_name: Set("app".to_string()),
            repo_owner: Set("owner".to_string()),
            directory: Set(".".to_string()),
            main_branch: Set("main".to_string()),
            preset: Set(temps_entities::preset::Preset::Static),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("insert project");
        let environment = environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("production".to_string()),
            slug: Set("production".to_string()),
            subdomain: Set("big-app".to_string()),
            host: Set("big-app.local".to_string()),
            upstreams: Set(UpstreamList::default()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("insert environment");
        let (project_id, environment_id) = (project.id, environment.id);
        let running_replica = |slug: &'static str, snapshot: DeploymentConfigSnapshot| {
            let db = db.clone();
            async move {
                let deployment = deployments::ActiveModel {
                    project_id: Set(project_id),
                    environment_id: Set(environment_id),
                    slug: Set(slug.to_string()),
                    state: Set("completed".to_string()),
                    metadata: Set(Some(deployments::DeploymentMetadata::default())),
                    deployment_config: Set(Some(snapshot)),
                    created_at: Set(Utc::now()),
                    updated_at: Set(Utc::now()),
                    ..Default::default()
                }
                .insert(db.as_ref())
                .await
                .expect("insert deployment");
                deployment_containers::ActiveModel {
                    deployment_id: Set(deployment.id),
                    container_id: Set(format!("{slug}-container")),
                    container_name: Set(format!("{slug}-1")),
                    container_port: Set(Some(3000)),
                    status: Set(Some("running".to_string())),
                    node_id: Set(None),
                    deployed_at: Set(Utc::now()),
                    created_at: Set(Utc::now()),
                    ..Default::default()
                }
                .insert(db.as_ref())
                .await
                .expect("insert container");
            }
        };
        // Request wins over limit, as the scheduler reserved it in flight.
        running_replica(
            "two-gib",
            DeploymentConfigSnapshot {
                cpu_request: Some(500_000),
                memory_request: Some(2048),
                memory_limit: Some(4096),
                ..Default::default()
            },
        )
        .await;

        let node_service = Arc::new(NodeService::new(db.clone()));
        let legacy = NodeScheduler::new(node_service.clone());
        let service = WorkloadPlacementService::new(
            db.clone(),
            node_service,
            proof_config_service(db.clone()),
            &legacy,
        )
        .with_control_plane_usage(Arc::new(|| {
            Some(NodeUsage {
                cpu_percent: None,
                memory_used_bytes: None,
                memory_total_bytes: Some(4.0 * 1024.0 * 1024.0 * 1024.0),
                cpu_cores: Some(4.0),
            })
        }));
        service
            .set_node_resources(
                CONTROL_PLANE_NODE_ID,
                NodeResourceUpdate {
                    resource_policy: Some(NodeResourcePolicy::Enforced),
                    system_reserved: Some(SystemReserved {
                        cpu_millis: 250,
                        memory_mb: 512,
                    }),
                },
            )
            .await
            .expect("enforced with a 512 MiB reserve");

        let nodes = service.node_facts(None).await.expect("facts");
        assert_eq!(nodes[0].id, CONTROL_PLANE_NODE_ID);
        assert_eq!(nodes[0].committed.memory_mb, 2048, "the configured request");
        assert_eq!(nodes[0].committed.cpu_millis, 500, "500_000 microcores");

        let placement_spec = temps_workload::PlacementSpec::default();
        let second_app = temps_workload::Resources {
            requests: Some(ResourceAmounts {
                cpu_millis: None,
                memory_mb: Some(2048),
                disk_mb: None,
                pids: None,
            }),
            limits: None,
        };
        let scheduler = temps_workload_plane::scheduler::UnifiedScheduler::new(
            service.scheduler_config().clone(),
        );
        let request = temps_workload_plane::scheduler::ScheduleRequest {
            kind: temps_workload::WorkloadKind::Deployment,
            placement: &placement_spec,
            resources: &second_app,
            isolation: Isolation::Process,
            image_platforms: &[],
            socket_gate: None,
            exclude_node_ids: &[],
            exclude_control_plane: false,
            affinity_nodes: &[],
        };
        assert!(
            scheduler.schedule(&nodes, &request).is_err(),
            "4096 - 512 reserved - 2048 committed leaves 1536 MiB: a second 2 GiB app must not fit"
        );

        // A limit-only deployment reserves its limit; one with nothing
        // configured reserves the deployment default.
        running_replica(
            "limit-only",
            DeploymentConfigSnapshot {
                memory_limit: Some(256),
                ..Default::default()
            },
        )
        .await;
        running_replica("unconfigured", DeploymentConfigSnapshot::default()).await;
        let default =
            temps_workload_plane::scheduler::kind_default_reservation(WorkloadKind::Deployment);
        let nodes = service.node_facts(None).await.expect("facts");
        assert_eq!(nodes[0].committed.memory_mb, 2048 + 256 + default.memory_mb);
        assert_eq!(nodes[0].committed.cpu_millis, 500 + 2 * default.cpu_millis);
        assert_eq!(nodes[0].live_instances, 3);
    }

    fn facts(id: i32, name: &str, cpu: f64, mem_used_gb: f64) -> NodeFacts {
        let mut node = NodeFacts::control_plane(None);
        node.id = id;
        node.name = name.to_string();
        node.system_reserved = SystemReserved::WORKER;
        node.usage = Some(NodeUsage {
            cpu_percent: Some(cpu),
            memory_used_bytes: Some(mem_used_gb * 1024.0 * 1024.0 * 1024.0),
            memory_total_bytes: Some(2.0 * 1024.0 * 1024.0 * 1024.0),
            cpu_cores: Some(2.0),
        });
        node
    }

    #[test]
    fn a_disagreement_is_attributed_to_the_reserve_only_when_removing_it_agrees() {
        let config = SchedulerConfig::new(temps_workload_plane::scheduler::platforms_equal);
        // 1.7 of 2 GB used and 85% CPU: under 0.90 without a reserve, over it
        // once 256 MB are kept for Temps.
        let nodes = vec![facts(1, "a", 85.0, 1.7), facts(2, "b", 95.0, 1.95)];
        let placement = PlacementSpec::default();
        let resources = Resources::default();
        let request = ScheduleRequest {
            kind: WorkloadKind::Deployment,
            placement: &placement,
            resources: &resources,
            isolation: Isolation::Process,
            image_platforms: &[],
            socket_gate: None,
            exclude_node_ids: &[],
            exclude_control_plane: false,
            affinity_nodes: &[],
        };
        let scheduler = UnifiedScheduler::new(config.clone());
        let agreeing = compare_placements(
            ShadowOutcome::from_unified(&scheduler.schedule(&nodes, &request)),
            &config,
            &nodes,
            &request,
            &UnifiedScheduler::new(config.clone()),
        );
        assert!(agreeing.agrees());

        // NodeScheduler's answer is the unified one without the reserve.
        let unreserved = without(&nodes, ExpectedDifference::SystemReserved);
        let legacy = ShadowOutcome::from_unified(
            &UnifiedScheduler::new(config.clone()).schedule(&unreserved, &request),
        );
        let comparison = compare_placements(
            legacy.clone(),
            &config,
            &nodes,
            &request,
            &UnifiedScheduler::new(config.clone()),
        );
        if comparison.legacy != comparison.unified {
            assert_eq!(
                comparison.disagreement,
                Some(Disagreement::Expected {
                    difference: ExpectedDifference::SystemReserved
                })
            );
        }

        // An answer nothing explains is unexpected.
        let comparison = compare_placements(
            ShadowOutcome::Placed { nodes: vec![77] },
            &config,
            &nodes,
            &request,
            &UnifiedScheduler::new(config.clone()),
        );
        assert!(comparison.is_unexpected());
        assert!(describe_shadow(&comparison).starts_with("WARNING"));
    }

    // ── The Phase 5 proof: a shadow run over a seeded cluster ───────────

    struct SeedNode {
        name: &'static str,
        status: &'static str,
        heartbeat_age_secs: i64,
        architecture: &'static str,
        labels: serde_json::Value,
        capacity: serde_json::Value,
    }

    fn capacity(cpu_percent: f64, used_gb: f64, total_gb: f64, cores: u32) -> serde_json::Value {
        let gb = 1024.0 * 1024.0 * 1024.0;
        serde_json::json!({
            "cpu_percent": cpu_percent,
            "memory_used_bytes": used_gb * gb,
            "memory_total_bytes": total_gb * gb,
            "cpu_cores": cores,
        })
    }

    fn seed_nodes() -> Vec<SeedNode> {
        let node = |name, status, age, arch, labels, capacity| SeedNode {
            name,
            status,
            heartbeat_age_secs: age,
            architecture: arch,
            labels,
            capacity,
        };
        vec![
            node(
                "a-worker",
                "active",
                5,
                "linux/amd64",
                serde_json::json!({"region": "eu", "tier": 2}),
                capacity(30.0, 2.0, 8.0, 4),
            ),
            node(
                "b-worker",
                "active",
                5,
                "linux/arm64",
                serde_json::json!({"region": "us"}),
                capacity(10.0, 1.0, 8.0, 4),
            ),
            node(
                "c-draining",
                "draining",
                5,
                "linux/amd64",
                serde_json::json!({"region": "eu"}),
                capacity(5.0, 1.0, 8.0, 4),
            ),
            node(
                "d-stale",
                "active",
                600,
                "linux/amd64",
                serde_json::json!({"region": "eu"}),
                capacity(5.0, 1.0, 8.0, 4),
            ),
            node(
                "e-dedicated",
                "active",
                5,
                "linux/amd64",
                serde_json::json!({"region": "eu", "temps.sh/role": "dedicated"}),
                capacity(20.0, 2.0, 8.0, 4),
            ),
            node(
                "f-builder",
                "active",
                5,
                "linux/amd64",
                serde_json::json!({"temps.sh/role": "builder"}),
                capacity(5.0, 1.0, 8.0, 4),
            ),
            // Under the 0.90 threshold on raw capacity, over it once the
            // 250m / 256 MB worker reserve is kept back (ADR-055 §5).
            node(
                "g-tight",
                "active",
                5,
                "linux/amd64",
                serde_json::json!({"region": "ap"}),
                capacity(84.0, 1.75, 2.0, 2),
            ),
            // Lightly loaded but small (1 core, 1 GB) and enforced: a
            // 1.5 GB limit does not fit it.
            node(
                "h-enforced",
                "active",
                5,
                "linux/amd64",
                serde_json::json!({"region": "ap"}),
                capacity(20.0, 0.4, 1.0, 1),
            ),
            // Busy but large (16 cores, 8 GB): the reserve barely moves its
            // score, so it stays under 0.90 where g-tight does not.
            node(
                "j-busy-large",
                "active",
                5,
                "linux/amd64",
                serde_json::json!({"region": "sa"}),
                capacity(87.0, 6.9, 8.0, 16),
            ),
            node(
                "i-offline",
                "offline",
                5,
                "linux/amd64",
                serde_json::json!({"region": "us"}),
                capacity(1.0, 1.0, 8.0, 4),
            ),
        ]
    }

    async fn insert_nodes(db: &DatabaseConnection) -> Result<BTreeMap<&'static str, i32>, DbErr> {
        let mut ids = BTreeMap::new();
        for seed in seed_nodes() {
            let now = Utc::now();
            let mut row = node_row(0, seed.name, "worker");
            let active = nodes::ActiveModel {
                name: Set(seed.name.to_string()),
                token_hash: Set(format!("hash-{}", seed.name)),
                address: Set(row.address.clone()),
                private_address: Set(row.private_address.clone()),
                role: Set("worker".to_string()),
                status: Set(seed.status.to_string()),
                labels: Set(seed.labels),
                capacity: Set(seed.capacity),
                architecture: Set(Some(seed.architecture.to_string())),
                last_heartbeat: Set(Some(
                    now - chrono::Duration::seconds(seed.heartbeat_age_secs),
                )),
                public_ingress_unsupported_reasons: Set(serde_json::json!([])),
                dns_resolver_consecutive_failures: Set(0),
                public_ingress_enabled: Set(false),
                created_at: Set(now),
                updated_at: Set(now),
                ..Default::default()
            };
            row = active.insert(db).await?;
            ids.insert(seed.name, row.id);
        }
        Ok(ids)
    }

    fn proof_config_service(db: Arc<DatabaseConnection>) -> Arc<temps_config::ConfigService> {
        let server_config = Arc::new(temps_config::ServerConfig {
            address: "127.0.0.1:3000".to_string(),
            database_url: "postgres://test".to_string(),
            tls_address: None,
            console_address: "127.0.0.1:0".to_string(),
            console_admin_address: None,
            console_ingress_address: None,
            admin_allowed_ips: Vec::new(),
            admin_allowed_hosts: Vec::new(),
            admin_trust_forwarded_for: false,
            data_dir: std::path::PathBuf::from("/tmp/temps-adr055-p5-test"),
            auth_secret: "test-secret".to_string(),
            encryption_key: "test-key".to_string(),
            api_base_url: "/api".to_string(),
            postgres_max_connections: None,
            postgres_min_connections: None,
            postgres_connect_timeout_secs: None,
            postgres_acquire_timeout_secs: None,
            postgres_idle_timeout_secs: None,
            postgres_max_lifetime_secs: None,
            clickhouse_url: None,
            clickhouse_database: None,
            clickhouse_user: None,
            clickhouse_password: None,
            docker_extra_networks: Vec::new(),
        });
        Arc::new(temps_config::ConfigService::new(server_config, db))
    }

    /// One seeded environment: what its project and environment configure.
    struct SeedEnvironment {
        name: &'static str,
        project: DeploymentConfig,
        environment: Option<DeploymentConfig>,
        exclude: Vec<&'static str>,
        image_platforms: Vec<String>,
        /// The documented difference this case is built to show, if any.
        expect: Option<ExpectedDifference>,
    }

    fn seed_environments(ids: &BTreeMap<&'static str, i32>) -> Vec<SeedEnvironment> {
        let id = |name: &str| ids[name];
        let cfg = |nodes: Option<Vec<i32>>,
                   labels: Option<serde_json::Value>,
                   spread: bool,
                   replicas: i32| {
            DeploymentConfig {
                target_nodes: nodes,
                target_labels: labels,
                anti_affinity: spread,
                replicas,
                ..Default::default()
            }
        };
        let env = |name, project, environment| SeedEnvironment {
            name,
            project,
            environment,
            exclude: Vec::new(),
            image_platforms: Vec::new(),
            expect: None,
        };
        let mut set = vec![
            env("unpinned", cfg(None, None, true, 1), None),
            env("unpinned-three-spread", cfg(None, None, true, 3), None),
            env("unpinned-two-packed", cfg(None, None, false, 2), None),
            env(
                "pinned-one",
                cfg(Some(vec![id("a-worker")]), None, true, 1),
                None,
            ),
            env(
                "pinned-two-spread",
                cfg(Some(vec![id("a-worker"), id("b-worker")]), None, true, 2),
                None,
            ),
            env(
                "pinned-two-packed",
                cfg(Some(vec![id("a-worker"), id("b-worker")]), None, false, 4),
                None,
            ),
            env(
                "pinned-dedicated",
                cfg(Some(vec![id("e-dedicated")]), None, true, 1),
                None,
            ),
            env(
                "pinned-draining",
                cfg(Some(vec![id("c-draining")]), None, true, 1),
                None,
            ),
            env(
                "pinned-stale",
                cfg(Some(vec![id("d-stale")]), None, true, 1),
                None,
            ),
            env(
                "pinned-offline",
                cfg(Some(vec![id("i-offline")]), None, true, 1),
                None,
            ),
            env(
                "pinned-builder",
                cfg(Some(vec![id("f-builder")]), None, true, 1),
                None,
            ),
            env(
                "pinned-missing",
                cfg(Some(vec![9_999]), None, true, 1),
                None,
            ),
            env(
                "labels-eu",
                cfg(None, Some(serde_json::json!({"region": "eu"})), true, 1),
                None,
            ),
            env(
                "labels-any-of",
                cfg(
                    None,
                    Some(serde_json::json!({"region": ["us", "eu"]})),
                    true,
                    2,
                ),
                None,
            ),
            env(
                "labels-number",
                cfg(None, Some(serde_json::json!({"tier": 2})), true, 1),
                None,
            ),
            env(
                "labels-none-match",
                cfg(None, Some(serde_json::json!({"region": "mars"})), true, 1),
                None,
            ),
            env(
                "labels-dedicated-only",
                cfg(
                    None,
                    Some(serde_json::json!({"temps.sh/role": "dedicated"})),
                    true,
                    1,
                ),
                None,
            ),
            // The environment overrides the project's pin.
            env(
                "env-overrides-project",
                cfg(Some(vec![id("b-worker")]), None, true, 1),
                Some(cfg(Some(vec![id("a-worker")]), None, true, 1)),
            ),
            // The environment clears nothing and inherits the labels.
            env(
                "env-inherits-labels",
                cfg(None, Some(serde_json::json!({"region": "us"})), true, 1),
                Some(cfg(Some(vec![]), Some(serde_json::json!({})), true, 1)),
            ),
        ];
        // Packing two replicas ranks the whole unpinned pool, and the
        // per-replica bump is spread over the pool size. g-tight is in
        // NodeScheduler's pool (0.854 raw) but over 0.90 after its reserve,
        // so the unified pool is one node smaller and the second replica's
        // bump differs: b-worker twice versus b-worker then a-worker. The
        // reserve is the only cause (the attribution re-runs without it).
        for seed in set
            .iter_mut()
            .filter(|seed| seed.name == "unpinned-two-packed")
        {
            seed.expect = Some(ExpectedDifference::SystemReserved);
        }
        // Alone, g-tight is used by both: when every candidate is over the
        // threshold both schedulers fall back to the whole pool.
        set.push(env(
            "pinned-tight",
            cfg(Some(vec![id("g-tight")]), None, true, 1),
            None,
        ));
        // Next to a large busy node it is not: raw, g-tight is the less
        // loaded (NodeScheduler takes it); after its reserve it is over
        // 0.90 and the unified scheduler takes j-busy-large.
        let mut tight = env(
            "pinned-tight-or-large",
            cfg(Some(vec![id("g-tight"), id("j-busy-large")]), None, true, 1),
            None,
        );
        tight.expect = Some(ExpectedDifference::SystemReserved);
        set.push(tight);
        let mut enforced = env(
            "pinned-enforced-full",
            DeploymentConfig {
                // More than the node has at all, so only the policy (not
                // the reserve) explains the refusal.
                memory_limit: Some(1536),
                cpu_limit: Some(1_500_000),
                ..cfg(Some(vec![id("h-enforced")]), None, true, 1)
            },
            None,
        );
        enforced.expect = Some(ExpectedDifference::EnforcedCapacity);
        set.push(enforced);
        // The previous revision's nodes are avoided, so the third replica
        // reaches the loaded end of the pool: g-tight (0.854 raw) for
        // NodeScheduler, j-busy-large for the unified scheduler, which
        // finds g-tight over 0.90 after its reserve.
        let mut excluded = env("spread-excluding-previous", cfg(None, None, true, 3), None);
        excluded.exclude = vec!["a-worker", "b-worker"];
        excluded.expect = Some(ExpectedDifference::SystemReserved);
        set.push(excluded);
        let mut arm = env("arm64-image", cfg(None, None, true, 1), None);
        arm.image_platforms = vec!["linux/arm64".to_string()];
        set.push(arm);
        let mut arm_two = env("arm64-image-two-spread", cfg(None, None, true, 2), None);
        arm_two.image_platforms = vec!["linux/arm64".to_string()];
        set.push(arm_two);
        set
    }

    /// ADR-055 Phase 5 proof. Seeds a varied cluster into a real database
    /// and, for every seeded environment, asks a fresh `NodeScheduler` and
    /// a fresh unified scheduler the same question. They must agree, or
    /// disagree only for a documented reason (`ExpectedDifference`), and
    /// each case built to show one must be attributed to exactly it.
    #[tokio::test]
    async fn shadow_scheduler_agrees_with_node_scheduler_except_for_documented_differences() {
        let test_db = match temps_database::test_utils::TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(e) => {
                println!("Postgres unavailable, skipping the shadow scheduler proof: {e}");
                return;
            }
        };
        let db = test_db.connection_arc();
        let ids = insert_nodes(db.as_ref()).await.expect("seed nodes");
        let node_service = Arc::new(NodeService::new(db.clone()));
        let config_service = proof_config_service(db.clone());
        let legacy_template = NodeScheduler::new(node_service.clone());
        let service = WorkloadPlacementService::new(
            db.clone(),
            node_service.clone(),
            config_service,
            &legacy_template,
        );
        service
            .set_node_resources(
                ids["h-enforced"],
                NodeResourceUpdate {
                    resource_policy: Some(NodeResourcePolicy::Enforced),
                    system_reserved: None,
                },
            )
            .await
            .expect("enforce h-enforced");

        let mut agreed = 0;
        let mut expected = 0;
        let mut report = Vec::new();
        let mut failures = Vec::new();
        for seed in seed_environments(&ids) {
            let placement = placement_spec_for(
                seed.environment.as_ref(),
                Some(&seed.project),
                seed.environment
                    .as_ref()
                    .map(|c| c.replicas)
                    .unwrap_or(seed.project.replicas)
                    .max(1) as u32,
            )
            .expect("placement");
            let labels = [seed.environment.as_ref(), Some(&seed.project)]
                .into_iter()
                .flatten()
                .find_map(|c| c.configured_target_labels().cloned());
            let resources = resources_for(seed.environment.as_ref(), Some(&seed.project));
            let exclude: Vec<i32> = seed.exclude.iter().map(|name| ids[name]).collect();
            // A fresh NodeScheduler per environment: its round-robin cursor
            // starts at 0, like the unified scheduler's.
            let legacy = NodeScheduler::new(node_service.clone());
            let comparison = service
                .shadow_run(
                    &legacy,
                    None,
                    &ShadowRequest {
                        placement: &placement,
                        target_labels: labels.as_ref(),
                        resources: &resources,
                        exclude_node_ids: &exclude,
                        image_platforms: &seed.image_platforms,
                        project_slug: Some("shop"),
                    },
                )
                .await
                .expect("shadow run");
            report.push(format!(
                "{:<28} legacy={:?} unified={:?} -> {:?}",
                seed.name, comparison.legacy, comparison.unified, comparison.disagreement
            ));
            match (seed.expect, &comparison.disagreement) {
                (None, None) => agreed += 1,
                (Some(want), Some(Disagreement::Expected { difference }))
                    if *difference == want =>
                {
                    expected += 1
                }
                (want, got) => failures.push(format!(
                    "{}: wanted {want:?}, got {got:?} ({comparison:?})",
                    seed.name
                )),
            }
        }
        println!("{}", report.join("\n"));
        assert!(
            failures.is_empty(),
            "shadow comparison failures:\n{}",
            failures.join("\n")
        );
        println!(
            "shadow scheduler proof: {} environments, {agreed} agree, {expected} differ as \
             documented, 0 unexpected",
            agreed + expected
        );
        assert!(agreed >= 20, "the bulk of the set must agree outright");
    }

    /// ADR-055 Phase 10: the per-environment isolation defaults to
    /// `process`, a write returns the previous value, and the explain
    /// endpoint refuses `micro_vm` with the scheduler's reason until the
    /// control plane reports a driver that provides it.
    #[tokio::test]
    async fn environment_isolation_round_trips_and_explains_the_micro_vm_refusal() {
        use temps_entities::upstream_config::UpstreamList;
        let test_db = match temps_database::test_utils::TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(e) => {
                println!("Postgres unavailable, skipping the isolation round trip: {e}");
                return;
            }
        };
        let db = test_db.connection_arc();
        let project = projects::ActiveModel {
            name: Set("isolated-app".to_string()),
            slug: Set("isolated-app".to_string()),
            repo_name: Set("app".to_string()),
            repo_owner: Set("owner".to_string()),
            directory: Set(".".to_string()),
            main_branch: Set("main".to_string()),
            preset: Set(temps_entities::preset::Preset::Static),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("insert project");
        let environment = environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("production".to_string()),
            slug: Set("production".to_string()),
            subdomain: Set("isolated-app".to_string()),
            host: Set("isolated-app.local".to_string()),
            upstreams: Set(UpstreamList::default()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("insert environment");
        let node_service = Arc::new(NodeService::new(db.clone()));
        let legacy = NodeScheduler::new(node_service.clone());
        let service = WorkloadPlacementService::new(
            db.clone(),
            node_service,
            proof_config_service(db.clone()),
            &legacy,
        );

        assert_eq!(
            service
                .environment_workload_isolation(environment.id)
                .await
                .expect("read default"),
            EnvironmentWorkloadIsolation::Process
        );
        let previous = service
            .set_environment_workload_isolation(
                environment.id,
                EnvironmentWorkloadIsolation::MicroVm,
            )
            .await
            .expect("set micro_vm");
        assert_eq!(previous, EnvironmentWorkloadIsolation::Process);
        assert_eq!(
            service
                .environment_workload_isolation(environment.id)
                .await
                .expect("read back"),
            EnvironmentWorkloadIsolation::MicroVm
        );
        // Writing an unrelated column through the full entity keeps it.
        service
            .set_environment_workload_mode(environment.id, EnvironmentWorkloadMode::Enforce)
            .await
            .expect("set enforce");
        assert_eq!(
            service
                .environment_workload_isolation(environment.id)
                .await
                .expect("read after mode change"),
            EnvironmentWorkloadIsolation::MicroVm
        );

        let (refused, _) = service
            .explain_environment(environment.id)
            .await
            .expect("explain without a vm driver");
        assert_eq!(refused.placement, None, "{refused:?}");
        let exclusion = refused
            .excluded
            .iter()
            .find(|e| e.node_id == CONTROL_PLANE_NODE_ID)
            .expect("the control plane is excluded");
        assert_eq!(exclusion.reason.code(), "DriverUnsupported");
        assert!(exclusion.to_string().contains("micro_vm"), "{exclusion}");

        service.set_control_plane_isolations(vec![Isolation::Process, Isolation::MicroVm]);
        let (placed, _) = service
            .explain_environment(environment.id)
            .await
            .expect("explain with a vm driver");
        assert_eq!(
            placed.placement,
            Some(vec![CONTROL_PLANE_NODE_ID]),
            "{placed:?}"
        );

        let missing = service
            .set_environment_workload_isolation(i32::MAX, EnvironmentWorkloadIsolation::MicroVm)
            .await
            .expect_err("unknown environment");
        assert!(matches!(
            missing,
            WorkloadPlacementError::EnvironmentNotFound { environment_id } if environment_id == i32::MAX
        ));
    }
}
