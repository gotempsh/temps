// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The unified scheduler (ADR-055 §5, §8; Phase 5).
//!
//! Pure: it takes what is known about every node ([`NodeFacts`]) and one
//! request ([`ScheduleRequest`]) and returns where each replica goes, or a
//! typed reason it cannot be placed ([`ScheduleError`]). No I/O, so the same
//! function backs placement, the shadow comparison with `NodeScheduler`, and
//! [`explain`].
//!
//! # Filters, in order
//!
//! A node is dropped by the first filter it fails, and the reason is kept:
//!
//! 1. **status**: not `active`, or no heartbeat within the threshold;
//! 2. **draining**;
//! 3. **operator allow-list** ([`SchedulerConfig::allowed_nodes`]);
//! 4. **role**: build-only nodes never host replicas; a dedicated node
//!    (`temps.sh/role=dedicated`) only hosts placements that pin it by id;
//! 5. **labels** (`target_labels` semantics: keys AND, array values OR);
//! 6. **architecture**: no image for the node's platform (an unreported
//!    platform keeps the node, with a warning);
//! 7. **driver capability**: the node's runtime cannot provide the
//!    requested isolation;
//! 8. **socket grant** (ADR-045): the project is declared and this node does
//!    not grant it;
//! 9. **capacity on enforcing nodes**: the reservations, plus this
//!    placement, exceed capacity minus `system_reserved`.
//!
//! The errors are decided on the sets `NodeScheduler` decided them on: a node
//! the label selector excludes counts as never considered (as it is dropped
//! silently there), so a builder or dedicated node outside the selector does
//! not turn "no node matches" into a role error.
//!
//! # Then rank, per kind
//!
//! - deployments and functions: least loaded first, with today's 0.90 load
//!   threshold over the capacity left after `system_reserved`, spread when
//!   anti-affinity is on; round-robin when no node reports its load;
//! - services: always spread;
//! - sandboxes: fewest live instances first;
//! - jobs: the owner's node first ([`ScheduleRequest::affinity_nodes`]).
//!
//! # Strict pinning
//!
//! A placement never leaves its `nodes` set or its label selector: when they
//! cannot be met the result is an error (`Scheduled=False` with the reason),
//! never a quiet move to another node. The legacy deploy path degraded some
//! scheduling failures to the control plane; this scheduler has no fallback.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde::Serialize;
use temps_entities::node_resource_settings::{NodeResourcePolicy, SystemReserved};
use temps_workload::{Isolation, PlacementSpec, Resources, WorkloadKind};

/// The control plane, which has no `nodes` row.
pub const CONTROL_PLANE_NODE_ID: i32 = 0;
/// Nodes at or above this load score are passed over, unless every node is.
pub const DEFAULT_MAX_LOAD_THRESHOLD: f64 = 0.90;
/// The node label that carries a scheduling role.
pub const ROLE_LABEL: &str = "temps.sh/role";
pub const BUILDER_ROLE: &str = "builder";
pub const DEDICATED_ROLE: &str = "dedicated";

const BYTES_PER_MB: f64 = 1024.0 * 1024.0;

/// A node's scheduling role, from its `temps.sh/role` label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeRole {
    Ordinary,
    /// Builds images and never hosts a replica.
    Builder,
    /// Hosts only placements that pin it by id.
    Dedicated,
}

impl NodeRole {
    /// The role named by a `temps.sh/role` label value.
    pub fn from_label(value: Option<&str>) -> Self {
        match value {
            Some(BUILDER_ROLE) => NodeRole::Builder,
            Some(DEDICATED_ROLE) => NodeRole::Dedicated,
            _ => NodeRole::Ordinary,
        }
    }
}

/// Usage a node reported in its last heartbeat. Every field is optional:
/// an agent reports what it can.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct NodeUsage {
    /// 0-100, across all cores.
    pub cpu_percent: Option<f64>,
    pub memory_used_bytes: Option<f64>,
    pub memory_total_bytes: Option<f64>,
    /// Number of CPU cores. Needed to deduct the CPU reserve and to enforce
    /// CPU reservations.
    pub cpu_cores: Option<f64>,
}

impl NodeUsage {
    /// Read the heartbeat `capacity` JSON (`cpu_percent`,
    /// `memory_used_bytes`, `memory_total_bytes`, `cpu_cores`).
    pub fn from_capacity_json(capacity: &serde_json::Value) -> Option<Self> {
        let object = capacity.as_object()?;
        let number = |key: &str| object.get(key).and_then(serde_json::Value::as_f64);
        let usage = NodeUsage {
            cpu_percent: number("cpu_percent"),
            memory_used_bytes: number("memory_used_bytes"),
            memory_total_bytes: number("memory_total_bytes"),
            cpu_cores: number("cpu_cores").filter(|cores| *cores > 0.0),
        };
        (usage != NodeUsage::default()).then_some(usage)
    }
}

/// CPU and memory held on a node: reservations of what runs there, or of
/// placements decided but not running yet.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Reservation {
    pub cpu_millis: u64,
    pub memory_mb: u64,
}

/// Saturating: a sum of reservations never wraps.
impl std::ops::Add for Reservation {
    type Output = Reservation;

    fn add(self, other: Reservation) -> Reservation {
        Reservation {
            cpu_millis: self.cpu_millis.saturating_add(other.cpu_millis),
            memory_mb: self.memory_mb.saturating_add(other.memory_mb),
        }
    }
}

impl fmt::Display for Reservation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}m CPU / {} MB", self.cpu_millis, self.memory_mb)
    }
}

/// What the kind is assumed to reserve when its spec sets neither a request
/// nor a limit (ADR-055 §5: "counted at its kind's default"). Estimates; the
/// console marks a value taken from here as assumed.
pub fn kind_default_reservation(kind: WorkloadKind) -> Reservation {
    match kind {
        WorkloadKind::Deployment => Reservation {
            cpu_millis: 250,
            memory_mb: 512,
        },
        WorkloadKind::Job => Reservation {
            cpu_millis: 250,
            memory_mb: 256,
        },
        WorkloadKind::Sandbox => Reservation {
            cpu_millis: 500,
            memory_mb: 512,
        },
        WorkloadKind::Service => Reservation {
            cpu_millis: 500,
            memory_mb: 1024,
        },
        WorkloadKind::Function => Reservation {
            cpu_millis: 100,
            memory_mb: 128,
        },
    }
}

/// What one instance of `resources` reserves, and whether the kind default
/// filled in for an unset value. A request wins, then a limit (what the
/// instance may use: the conservative reading for an existing deployment,
/// whose configured values are limits), then the kind default.
pub fn reservation_of(kind: WorkloadKind, resources: &Resources) -> (Reservation, bool) {
    let default = kind_default_reservation(kind);
    let pick = |request: Option<u32>, limit: Option<u32>| request.or(limit);
    let requests = resources.requests.unwrap_or_default();
    let limits = resources.limits.unwrap_or_default();
    let cpu = pick(requests.cpu_millis, limits.cpu_millis);
    let memory = pick(requests.memory_mb, limits.memory_mb);
    (
        Reservation {
            cpu_millis: cpu.map(u64::from).unwrap_or(default.cpu_millis),
            memory_mb: memory.map(u64::from).unwrap_or(default.memory_mb),
        },
        cpu.is_none() || memory.is_none(),
    )
}

/// Everything the scheduler knows about one node, node 0 included.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NodeFacts {
    pub id: i32,
    pub name: String,
    /// `active`, `draining`, `offline`, `pending`.
    pub status: String,
    /// Whether the last heartbeat is within the threshold. Always `true`
    /// for the control plane.
    pub heartbeat_fresh: bool,
    pub role: NodeRole,
    /// Label values as strings (`target_labels` compares them as strings).
    pub labels: BTreeMap<String, String>,
    /// OCI platform (`linux/amd64`); `None` = not reported.
    pub architecture: Option<String>,
    /// Isolations the node's runtime provides. Empty = not reported, read
    /// as process isolation (every node runs Docker today).
    pub isolations: Vec<Isolation>,
    /// Whether this node grants the requested project the Docker socket
    /// (ADR-045). Only read when the request carries a socket gate.
    pub grants_socket: bool,
    pub usage: Option<NodeUsage>,
    pub resource_policy: NodeResourcePolicy,
    pub system_reserved: SystemReserved,
    /// Reservations of what already runs on the node.
    pub committed: Reservation,
    /// Reservations of placements decided but not running yet.
    pub in_flight: Reservation,
    /// Instances running on the node (for sandbox ranking).
    pub live_instances: u32,
}

/// The isolation as the API and the spec spell it (`micro_vm`).
pub fn isolation_name(isolation: Isolation) -> &'static str {
    match isolation {
        Isolation::InHost => "in_host",
        Isolation::Process => "process",
        Isolation::MicroVm => "micro_vm",
    }
}

impl NodeFacts {
    /// Whether the node's runtime provides `isolation`. A node that reported
    /// nothing runs Docker, which provides process isolation only: an
    /// unreported list must never admit a microVM request.
    pub fn provides(&self, isolation: Isolation) -> bool {
        if self.isolations.is_empty() {
            return isolation == Isolation::Process;
        }
        self.isolations.contains(&isolation)
    }

    /// The control plane as `NodeScheduler` sees it: active, ordinary, no
    /// labels, and no capacity report.
    pub fn control_plane(architecture: Option<String>) -> Self {
        NodeFacts {
            id: CONTROL_PLANE_NODE_ID,
            name: "control-plane".to_string(),
            status: "active".to_string(),
            heartbeat_fresh: true,
            role: NodeRole::Ordinary,
            labels: BTreeMap::new(),
            architecture,
            isolations: vec![Isolation::Process],
            grants_socket: false,
            usage: None,
            resource_policy: NodeResourcePolicy::BestEffort,
            system_reserved: SystemReserved::CONTROL_PLANE_LOCAL_DATABASE,
            committed: Reservation::default(),
            in_flight: Reservation::default(),
            live_instances: 0,
        }
    }

    fn is_control_plane(&self) -> bool {
        self.id == CONTROL_PLANE_NODE_ID
    }

    /// CPU capacity left after the reserve, in millicores, when the core
    /// count is known.
    pub fn schedulable_cpu_millis(&self) -> Option<f64> {
        let cores = self.usage?.cpu_cores?;
        Some(cores * 1000.0 - f64::from(self.system_reserved.cpu_millis))
    }

    /// Memory capacity left after the reserve, in MB, when known.
    pub fn schedulable_memory_mb(&self) -> Option<f64> {
        let total = self.usage?.memory_total_bytes?;
        Some(total / BYTES_PER_MB - f64::from(self.system_reserved.memory_mb))
    }

    /// Load score 0.0 (idle) to 1.0 (full) over the capacity left after
    /// `system_reserved`, counting in-flight memory as used. `None` when the
    /// node reports nothing usable. With no reserve and nothing in flight
    /// this is exactly `NodeScheduler`'s score: 60% CPU, 40% memory.
    pub fn load_score(&self) -> Option<f64> {
        let usage = self.usage?;
        let cpu_percent = usage.cpu_percent.map(|cpu| {
            match usage
                .cpu_cores
                .filter(|_| self.system_reserved.cpu_millis > 0)
            {
                Some(cores) => {
                    let total = cores * 1000.0;
                    let remaining = total - f64::from(self.system_reserved.cpu_millis);
                    if remaining <= 0.0 {
                        f64::INFINITY
                    } else {
                        cpu * (total / remaining)
                    }
                }
                // Without a core count the CPU reserve cannot be converted
                // to a share; it is not guessed.
                None => cpu,
            }
        });
        let memory_percent = match (usage.memory_used_bytes, usage.memory_total_bytes) {
            (Some(used), Some(total)) if total > 0.0 => {
                let reserve = f64::from(self.system_reserved.memory_mb) * BYTES_PER_MB;
                let in_flight = self.in_flight.memory_mb as f64 * BYTES_PER_MB;
                let remaining = total - reserve;
                if remaining <= 0.0 {
                    Some(f64::INFINITY)
                } else {
                    Some(((used + in_flight) / remaining) * 100.0)
                }
            }
            _ => None,
        };
        match (cpu_percent, memory_percent) {
            (Some(cpu), Some(memory)) => Some((cpu * 0.6 + memory * 0.4) / 100.0),
            (Some(cpu), None) => Some(cpu / 100.0),
            (None, Some(memory)) => Some(memory / 100.0),
            (None, None) => None,
        }
    }

    fn availability_problem(&self) -> Option<ExclusionReason> {
        if self.status == "draining" {
            return Some(ExclusionReason::Draining);
        }
        if self.status != "active" {
            return Some(ExclusionReason::Unavailable {
                state: self.status.clone(),
            });
        }
        if !self.heartbeat_fresh {
            return Some(ExclusionReason::Unavailable {
                state: "not sending heartbeats".to_string(),
            });
        }
        None
    }
}

/// The ADR-045 gate for one project: it exists only when the control plane
/// declares the project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SocketGate {
    pub project_slug: String,
    /// Whether the control plane's own environment grants it.
    pub control_plane_grants: bool,
}

/// One placement to decide.
#[derive(Debug, Clone)]
pub struct ScheduleRequest<'a> {
    pub kind: WorkloadKind,
    pub placement: &'a PlacementSpec,
    pub resources: &'a Resources,
    pub isolation: Isolation,
    /// Platforms an image exists for; empty = no constraint.
    pub image_platforms: &'a [String],
    pub socket_gate: Option<SocketGate>,
    /// Nodes that already host this environment: avoided while spreading,
    /// unless nothing else is left.
    pub exclude_node_ids: &'a [i32],
    /// The caller wants a worker (a build moved off the control plane).
    pub exclude_control_plane: bool,
    /// Preferred nodes (a job: its owner's node).
    pub affinity_nodes: &'a [i32],
}

/// How the scheduler is configured. Built once.
#[derive(Debug, Clone)]
pub struct SchedulerConfig {
    pub max_load_threshold: f64,
    /// Whether this process runs workloads on the control plane.
    pub local_workloads_enabled: bool,
    /// The control plane's platform; `None` = unknown (compatible).
    pub local_platform: Option<String>,
    /// Operator allow-list: when set, only these nodes are considered.
    pub allowed_nodes: Option<BTreeSet<i32>>,
    /// Whether an image built for the first platform runs on the second.
    pub platforms_match: fn(&str, &str) -> bool,
}

impl SchedulerConfig {
    pub fn new(platforms_match: fn(&str, &str) -> bool) -> Self {
        SchedulerConfig {
            max_load_threshold: DEFAULT_MAX_LOAD_THRESHOLD,
            local_workloads_enabled: true,
            local_platform: None,
            allowed_nodes: None,
            platforms_match,
        }
    }
}

/// Exact, case-insensitive OCI platform comparison: the fallback when the
/// caller has no richer matcher.
pub fn platforms_equal(image: &str, node: &str) -> bool {
    image.trim().eq_ignore_ascii_case(node.trim())
}

/// Why a node was passed over.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum ExclusionReason {
    /// Not active, or no recent heartbeat.
    Unavailable {
        state: String,
    },
    Draining,
    /// Pinned by id, but no such node exists.
    TargetMissing,
    /// Not on the operator allow-list.
    NotAllowed,
    BuildOnly,
    /// Dedicated, and this placement does not pin it.
    Dedicated,
    /// Its labels do not satisfy the selector.
    LabelMismatch,
    IncompatibleArchitecture {
        node_platform: String,
        image_platforms: Vec<String>,
    },
    /// Kept: its platform is not reported, so compatibility is assumed.
    UnverifiedArchitecture,
    DriverUnsupported {
        isolation: String,
    },
    SocketNotGranted {
        project_slug: String,
    },
    /// An enforcing node without the capacity for this placement.
    InsufficientCapacity {
        needed: Reservation,
        available_cpu_millis: i64,
        available_memory_mb: i64,
    },
    /// An enforcing node that does not report the capacity to check.
    CapacityUnknown {
        dimension: String,
    },
    /// The control plane cannot host it (local workloads are disabled, or
    /// the request excludes it, or it does not satisfy the constraints).
    ControlPlaneUnavailable {
        reason: String,
    },
}

impl ExclusionReason {
    /// Counted by the anti-affinity shortfall: permanent reasons. Load,
    /// availability and dedication are not (`NodeScheduler`'s rule).
    fn counts_toward_shortfall(&self) -> bool {
        matches!(
            self,
            ExclusionReason::TargetMissing
                | ExclusionReason::NotAllowed
                | ExclusionReason::BuildOnly
                | ExclusionReason::IncompatibleArchitecture { .. }
                | ExclusionReason::DriverUnsupported { .. }
                | ExclusionReason::SocketNotGranted { .. }
        )
    }

    /// The condition reason it maps to.
    pub fn code(&self) -> &'static str {
        match self {
            ExclusionReason::Unavailable { .. } => "NodeUnavailable",
            ExclusionReason::Draining => "NodeDraining",
            ExclusionReason::TargetMissing => "TargetNodeMissing",
            ExclusionReason::NotAllowed => "NodeNotAllowed",
            ExclusionReason::BuildOnly => "BuildOnlyNode",
            ExclusionReason::Dedicated => "DedicatedNode",
            ExclusionReason::LabelMismatch => "LabelMismatch",
            ExclusionReason::IncompatibleArchitecture { .. } => "IncompatibleArchitecture",
            ExclusionReason::UnverifiedArchitecture => "UnverifiedArchitecture",
            ExclusionReason::DriverUnsupported { .. } => "DriverUnsupported",
            ExclusionReason::SocketNotGranted { .. } => "DockerSocketNotGranted",
            ExclusionReason::InsufficientCapacity { .. } => "InsufficientCapacity",
            ExclusionReason::CapacityUnknown { .. } => "CapacityUnknown",
            ExclusionReason::ControlPlaneUnavailable { .. } => "ControlPlaneUnavailable",
        }
    }
}

impl fmt::Display for ExclusionReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExclusionReason::Unavailable { state } => write!(f, "is unavailable ({state})"),
            ExclusionReason::Draining => f.write_str("is draining"),
            ExclusionReason::TargetMissing => {
                f.write_str("is listed in target_nodes but does not exist")
            }
            ExclusionReason::NotAllowed => f.write_str("is not on the operator allow-list"),
            ExclusionReason::BuildOnly => {
                f.write_str("is a build-only node (temps.sh/role=builder)")
            }
            ExclusionReason::Dedicated => f.write_str(
                "is dedicated (temps.sh/role=dedicated) and only runs placements that pin it \
                 in target_nodes",
            ),
            ExclusionReason::LabelMismatch => f.write_str("does not match the label selector"),
            ExclusionReason::IncompatibleArchitecture {
                node_platform,
                image_platforms,
            } => write!(
                f,
                "runs {node_platform}, and the image exists only for {}",
                image_platforms.join(", ")
            ),
            ExclusionReason::UnverifiedArchitecture => {
                f.write_str("has not reported its architecture; assumed compatible")
            }
            ExclusionReason::DriverUnsupported { isolation } => {
                write!(f, "has no runtime that provides {isolation} isolation")
            }
            ExclusionReason::SocketNotGranted { project_slug } => write!(
                f,
                "does not grant project '{project_slug}' host Docker access (ADR 045)"
            ),
            ExclusionReason::InsufficientCapacity {
                needed,
                available_cpu_millis,
                available_memory_mb,
            } => write!(
                f,
                "enforces reservations and has {available_cpu_millis}m CPU / \
                 {available_memory_mb} MB left after system_reserved; this needs {needed}"
            ),
            ExclusionReason::CapacityUnknown { dimension } => write!(
                f,
                "enforces reservations but does not report its {dimension} capacity"
            ),
            ExclusionReason::ControlPlaneUnavailable { reason } => {
                write!(f, "cannot host it: {reason}")
            }
        }
    }
}

/// A node passed over, or kept with a warning (`excluded: false`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NodeExclusion {
    pub node_id: i32,
    pub node_name: String,
    pub reason: ExclusionReason,
    pub excluded: bool,
}

impl fmt::Display for NodeExclusion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "node {} ({}) {}",
            self.node_id, self.node_name, self.reason
        )
    }
}

/// Where each replica goes.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Placement {
    /// One node id per replica; `0` is the control plane.
    pub nodes: Vec<i32>,
    pub exclusions: Vec<NodeExclusion>,
    pub warnings: Vec<String>,
    /// Whether the ADR-045 gate applied.
    pub socket_required: bool,
    /// What each replica reserves.
    pub reservation: Reservation,
    /// Whether `reservation` came (partly) from the kind default.
    pub reservation_assumed: bool,
}

/// Why a placement cannot be made. Each becomes `Scheduled=False` with
/// [`ScheduleError::reason`] and this message.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ScheduleError {
    #[error(
        "the control plane does not run workloads (local workloads are disabled) and no \
         worker node can take the {replicas} replica(s); join a worker with `temps join`"
    )]
    LocalWorkloadsDisabled { replicas: u32 },
    #[error(
        "every node that could take it is dedicated and not pinned in target_nodes: {excluded}. \
         Pin one by id to use it"
    )]
    DedicatedNodesNotPinned { excluded: String },
    #[error("no node satisfies the placement constraints: {excluded}")]
    PlacementConstraintsUnsatisfied { excluded: String },
    #[error("anti-affinity needs {replicas} nodes, {available} can run it ({cause}): {excluded}")]
    InsufficientCompatibleNodes {
        replicas: u32,
        available: usize,
        cause: String,
        excluded: String,
    },
    #[error(
        "no node can run an image built for {image_platforms} (control plane: {local_platform})"
    )]
    NoCompatibleNode {
        image_platforms: String,
        local_platform: String,
    },
    /// ADR-055 Phase 10: every node was excluded because none has a driver
    /// for the requested isolation (`micro_vm` with no Firecracker host).
    #[error(
        "no node can run {isolation} isolation: {excluded}. Provision a node whose runtime \
         provides it, or set the environment's workload_isolation back to process"
    )]
    NoIsolationProvider { isolation: String, excluded: String },
    #[error("project '{project_slug}' needs the host Docker socket and {reason}")]
    DockerSocketNotSchedulable {
        project_slug: String,
        reason: String,
    },
    #[error(
        "{placed} of {replicas} replica(s) fit: enforcing nodes have no capacity left for {needed} \
         each: {excluded}"
    )]
    InsufficientCapacity {
        replicas: u32,
        placed: u32,
        needed: Reservation,
        excluded: String,
    },
}

impl ScheduleError {
    /// The `Scheduled` condition reason.
    pub fn reason(&self) -> &'static str {
        match self {
            ScheduleError::LocalWorkloadsDisabled { .. } => "LocalWorkloadsDisabled",
            ScheduleError::DedicatedNodesNotPinned { .. } => "DedicatedNodesNotPinned",
            ScheduleError::PlacementConstraintsUnsatisfied { .. } => {
                "PlacementConstraintsUnsatisfied"
            }
            ScheduleError::InsufficientCompatibleNodes { .. } => "InsufficientCompatibleNodes",
            ScheduleError::NoCompatibleNode { .. } => "NoCompatibleNode",
            ScheduleError::NoIsolationProvider { .. } => "NoIsolationProvider",
            ScheduleError::DockerSocketNotSchedulable { .. } => "DockerSocketNotSchedulable",
            ScheduleError::InsufficientCapacity { .. } => "InsufficientCapacity",
        }
    }
}

/// A failed placement with the exclusions computed on the way.
struct Rejected {
    error: ScheduleError,
    exclusions: Vec<NodeExclusion>,
}

impl Rejected {
    fn new(exclusions: &[NodeExclusion], error: ScheduleError) -> Self {
        Self {
            error,
            exclusions: exclusions.to_vec(),
        }
    }
}

/// The scheduler. Holds only the round-robin cursor, used when no node
/// reports its load (as `NodeScheduler` does).
#[derive(Debug)]
pub struct UnifiedScheduler {
    config: SchedulerConfig,
    cursor: AtomicUsize,
}

struct PoolEntry {
    node: usize,
    load: Option<f64>,
}

/// Remaining enforceable capacity of one node during assignment.
#[derive(Clone, Copy)]
struct Headroom {
    cpu_millis: Option<f64>,
    memory_mb: Option<f64>,
}

impl Headroom {
    fn fits(&self, need: Reservation) -> bool {
        self.cpu_millis
            .is_none_or(|cpu| cpu >= need.cpu_millis as f64)
            && self
                .memory_mb
                .is_none_or(|mem| mem >= need.memory_mb as f64)
    }

    fn take(&mut self, need: Reservation) {
        if let Some(cpu) = self.cpu_millis.as_mut() {
            *cpu -= need.cpu_millis as f64;
        }
        if let Some(mem) = self.memory_mb.as_mut() {
            *mem -= need.memory_mb as f64;
        }
    }
}

/// The capacity an enforcing node has left for new placements, or why it
/// cannot be checked. `None` dimensions are not enforced.
fn enforced_headroom(node: &NodeFacts, need: Reservation) -> Result<Headroom, ExclusionReason> {
    let held = node.committed + node.in_flight;
    let cpu = match node.schedulable_cpu_millis() {
        Some(capacity) => Some(capacity - held.cpu_millis as f64),
        None if need.cpu_millis > 0 => {
            return Err(ExclusionReason::CapacityUnknown {
                dimension: "CPU (core count)".to_string(),
            })
        }
        None => None,
    };
    let memory = match node.schedulable_memory_mb() {
        Some(capacity) => Some(capacity - held.memory_mb as f64),
        None if need.memory_mb > 0 => {
            return Err(ExclusionReason::CapacityUnknown {
                dimension: "memory".to_string(),
            })
        }
        None => None,
    };
    let headroom = Headroom {
        cpu_millis: cpu,
        memory_mb: memory,
    };
    if headroom.fits(need) {
        Ok(headroom)
    } else {
        Err(ExclusionReason::InsufficientCapacity {
            needed: need,
            available_cpu_millis: cpu.unwrap_or(0.0).floor() as i64,
            available_memory_mb: memory.unwrap_or(0.0).floor() as i64,
        })
    }
}

/// One phrase naming why the shortfall nodes were dropped.
fn shortfall_cause(exclusions: &[&NodeExclusion]) -> &'static str {
    let mut architecture = false;
    let mut socket = false;
    let mut build_only = false;
    let mut missing = false;
    let mut other = false;
    for exclusion in exclusions {
        match exclusion.reason {
            ExclusionReason::IncompatibleArchitecture { .. } => architecture = true,
            ExclusionReason::SocketNotGranted { .. } => socket = true,
            ExclusionReason::BuildOnly => build_only = true,
            ExclusionReason::TargetMissing => missing = true,
            _ => other = true,
        }
    }
    if missing {
        return if architecture || socket || build_only || other {
            "some are listed in target_nodes but no longer exist, and others cannot run it"
        } else {
            "they are listed in target_nodes but no longer exist"
        };
    }
    if build_only && !architecture && !socket && !other {
        return "they are build-only nodes (temps.sh/role=builder), which never host replicas";
    }
    match (architecture, socket) {
        (true, true) => {
            "some nodes have no image for their architecture and others do not grant this \
             project host Docker access (ADR 045)"
        }
        (true, false) => "there is no image for their architecture",
        (false, true) => "they do not grant this project host Docker access (ADR 045)",
        (false, false) => "they were dropped from the scheduling pool",
    }
}

fn join_exclusions(exclusions: &[&NodeExclusion]) -> String {
    exclusions
        .iter()
        .map(|e| e.to_string())
        .collect::<Vec<_>>()
        .join("; ")
}

fn spread_warning(nodes: &[i32], spread: bool) -> Option<String> {
    if !spread || nodes.len() < 2 {
        return None;
    }
    let distinct: BTreeSet<i32> = nodes.iter().copied().collect();
    (distinct.len() < nodes.len()).then(|| {
        format!(
            "anti-affinity is on, but {} replicas share {} node(s): not enough eligible nodes \
             for one replica each. Add an eligible node, lower the replica count, or check the \
             skipped nodes above",
            nodes.len(),
            distinct.len()
        )
    })
}

impl UnifiedScheduler {
    pub fn new(config: SchedulerConfig) -> Self {
        UnifiedScheduler {
            config,
            cursor: AtomicUsize::new(0),
        }
    }

    pub fn config(&self) -> &SchedulerConfig {
        &self.config
    }

    fn local_supports(&self, image_platforms: &[String]) -> bool {
        if image_platforms.is_empty() {
            return true;
        }
        match self.config.local_platform.as_deref() {
            Some(local) => image_platforms
                .iter()
                .any(|platform| (self.config.platforms_match)(platform, local)),
            None => true,
        }
    }

    /// The first filter `node` fails, in the module's order (the control
    /// plane's own rules are applied by [`Self::schedule`]).
    fn first_failure(
        &self,
        node: &NodeFacts,
        request: &ScheduleRequest<'_>,
        pinned: Option<&[i32]>,
    ) -> Option<ExclusionReason> {
        if let Some(problem) = node.availability_problem() {
            return Some(problem);
        }
        if let Some(allowed) = &self.config.allowed_nodes {
            if !allowed.contains(&node.id) {
                return Some(ExclusionReason::NotAllowed);
            }
        }
        match node.role {
            NodeRole::Builder => return Some(ExclusionReason::BuildOnly),
            NodeRole::Dedicated if !pinned.is_some_and(|ids| ids.contains(&node.id)) => {
                return Some(ExclusionReason::Dedicated);
            }
            NodeRole::Dedicated | NodeRole::Ordinary => {}
        }
        if !request.placement.labels_match(&node.labels) {
            return Some(ExclusionReason::LabelMismatch);
        }
        if !request.image_platforms.is_empty() {
            if let Some(platform) = node.architecture.as_deref() {
                if !request
                    .image_platforms
                    .iter()
                    .any(|image| (self.config.platforms_match)(image, platform))
                {
                    return Some(ExclusionReason::IncompatibleArchitecture {
                        node_platform: platform.to_string(),
                        image_platforms: request.image_platforms.to_vec(),
                    });
                }
            }
        }
        if !node.provides(request.isolation) {
            return Some(ExclusionReason::DriverUnsupported {
                isolation: isolation_name(request.isolation).to_string(),
            });
        }
        if let Some(gate) = &request.socket_gate {
            if !node.grants_socket {
                return Some(ExclusionReason::SocketNotGranted {
                    project_slug: gate.project_slug.clone(),
                });
            }
        }
        None
    }

    /// Decide where every replica of `request` runs. `nodes` is every node
    /// known, node 0 included (when absent, the control plane is not a
    /// candidate). Worker order is the caller's (by name, as `NodeScheduler`
    /// reads them).
    pub fn schedule(
        &self,
        nodes: &[NodeFacts],
        request: &ScheduleRequest<'_>,
    ) -> Result<Placement, ScheduleError> {
        self.schedule_traced(nodes, request)
            .map_err(|rejected| rejected.error)
    }

    /// `schedule`, keeping the per-node exclusions on failure so `explain`
    /// can show why each node was refused.
    fn schedule_traced(
        &self,
        nodes: &[NodeFacts],
        request: &ScheduleRequest<'_>,
    ) -> Result<Placement, Rejected> {
        let placement = request.placement;
        let replicas = placement.replicas.max(1);
        let pinned = placement.pinned_nodes();
        let targets_control_plane = pinned.is_some_and(|ids| ids.contains(&CONTROL_PLANE_NODE_ID));
        let has_label_constraints = placement.label_selector().is_some();
        let has_node_constraints = pinned.is_some() || has_label_constraints;
        let (need, assumed) = reservation_of(request.kind, request.resources);
        let spread = placement.spread || request.kind == WorkloadKind::Service;

        let mut exclusions: Vec<NodeExclusion> = Vec::new();
        // Whether the exclusion is one `NodeScheduler` would have seen
        // (the label selector matched the node).
        let mut visible: Vec<bool> = Vec::new();
        let mut eligible: Vec<usize> = Vec::new();
        // Nodes passing every filter up to and including the socket gate,
        // ignoring architecture, driver and capacity: the set on which the
        // socket gate's own error is decided.
        let mut socket_survivors = 0usize;
        let mut headroom: BTreeMap<usize, Headroom> = BTreeMap::new();

        for missing in pinned
            .into_iter()
            .flatten()
            .filter(|id| **id != CONTROL_PLANE_NODE_ID)
            .filter(|id| !nodes.iter().any(|node| node.id == **id))
            .collect::<BTreeSet<_>>()
        {
            exclusions.push(NodeExclusion {
                node_id: *missing,
                node_name: format!("node {missing}"),
                reason: ExclusionReason::TargetMissing,
                excluded: true,
            });
            visible.push(true);
        }

        for (index, node) in nodes.iter().enumerate() {
            if node.is_control_plane() {
                continue;
            }
            if pinned.is_some_and(|ids| !ids.contains(&node.id)) {
                continue;
            }
            let failure = self.first_failure(node, request, pinned);
            let matches_labels = placement.labels_match(&node.labels);
            let passes_to_socket = match &failure {
                None => true,
                Some(
                    ExclusionReason::IncompatibleArchitecture { .. }
                    | ExclusionReason::DriverUnsupported { .. },
                ) => {
                    // Failed a later filter; would it pass the socket gate?
                    request
                        .socket_gate
                        .as_ref()
                        .is_none_or(|_| node.grants_socket)
                }
                Some(_) => false,
            };
            if passes_to_socket {
                socket_survivors += 1;
            }
            let failure = match failure {
                Some(reason) => Some(reason),
                None if node.resource_policy == NodeResourcePolicy::Enforced => {
                    match enforced_headroom(node, need) {
                        Ok(room) => {
                            headroom.insert(index, room);
                            None
                        }
                        Err(reason) => Some(reason),
                    }
                }
                None => None,
            };
            match failure {
                Some(reason) => {
                    exclusions.push(NodeExclusion {
                        node_id: node.id,
                        node_name: node.name.clone(),
                        reason,
                        excluded: true,
                    });
                    visible.push(matches_labels);
                }
                None => {
                    if node.architecture.is_none() && !request.image_platforms.is_empty() {
                        exclusions.push(NodeExclusion {
                            node_id: node.id,
                            node_name: node.name.clone(),
                            reason: ExclusionReason::UnverifiedArchitecture,
                            excluded: false,
                        });
                        visible.push(true);
                    }
                    eligible.push(index);
                }
            }
        }

        // The control plane.
        let socket_required = request.socket_gate.is_some();
        let local_grants_socket = request
            .socket_gate
            .as_ref()
            .is_none_or(|gate| gate.control_plane_grants);
        let control_index = nodes.iter().position(NodeFacts::is_control_plane);
        let local_matches_constraints =
            !has_label_constraints && pinned.is_none_or(|_| targets_control_plane);
        let local_enabled = self.config.local_workloads_enabled && control_index.is_some();
        let mut include_local = !request.exclude_control_plane
            && local_enabled
            && self.local_supports(request.image_platforms)
            && local_matches_constraints
            && local_grants_socket;
        if let Some(index) = control_index.filter(|_| include_local) {
            let control = &nodes[index];
            let unified_only = if self
                .config
                .allowed_nodes
                .as_ref()
                .is_some_and(|allowed| !allowed.contains(&CONTROL_PLANE_NODE_ID))
            {
                Some(ExclusionReason::NotAllowed)
            } else if !control.provides(request.isolation) {
                Some(ExclusionReason::DriverUnsupported {
                    isolation: isolation_name(request.isolation).to_string(),
                })
            } else if control.resource_policy == NodeResourcePolicy::Enforced {
                match enforced_headroom(control, need) {
                    Ok(room) => {
                        headroom.insert(index, room);
                        None
                    }
                    Err(reason) => Some(reason),
                }
            } else {
                None
            };
            if let Some(reason) = unified_only {
                exclusions.push(NodeExclusion {
                    node_id: CONTROL_PLANE_NODE_ID,
                    node_name: control.name.clone(),
                    reason,
                    excluded: true,
                });
                visible.push(true);
                include_local = false;
            }
        }

        let hard: Vec<&NodeExclusion> = exclusions
            .iter()
            .zip(&visible)
            .filter(|(e, visible)| {
                **visible && e.excluded && e.reason != ExclusionReason::LabelMismatch
            })
            .map(|(e, _)| e)
            .collect();
        let shortfall: Vec<&NodeExclusion> = hard
            .iter()
            .copied()
            .filter(|e| e.reason.counts_toward_shortfall())
            .collect();

        if let Some(gate) = &request.socket_gate {
            if socket_survivors == 0 && !(local_enabled && local_grants_socket) {
                return Err(Rejected::new(
                    &exclusions,
                    ScheduleError::DockerSocketNotSchedulable {
                        project_slug: gate.project_slug.clone(),
                        reason: "no schedulable host grants it: the granting hosts are inactive, \
                             draining, dedicated to environments that pin them, or excluded by \
                             the requested node/label placement"
                            .to_string(),
                    },
                ));
            }
        }
        if targets_control_plane && !self.config.local_workloads_enabled {
            return Err(Rejected::new(
                &exclusions,
                ScheduleError::LocalWorkloadsDisabled { replicas },
            ));
        }
        if eligible.is_empty()
            && !include_local
            && hard.iter().any(|e| e.reason == ExclusionReason::Dedicated)
        {
            return Err(Rejected::new(
                &exclusions,
                ScheduleError::DedicatedNodesNotPinned {
                    excluded: join_exclusions(&hard),
                },
            ));
        }
        let capacity_bound = hard.iter().any(|e| {
            matches!(
                e.reason,
                ExclusionReason::InsufficientCapacity { .. }
                    | ExclusionReason::CapacityUnknown { .. }
            )
        });
        if eligible.is_empty() && !include_local && capacity_bound {
            return Err(Rejected::new(
                &exclusions,
                ScheduleError::InsufficientCapacity {
                    replicas,
                    placed: 0,
                    needed: need,
                    excluded: join_exclusions(&hard),
                },
            ));
        }
        if has_node_constraints && eligible.is_empty() && !include_local {
            let excluded = if hard.is_empty() {
                "no active node matched the requested node IDs or labels".to_string()
            } else {
                join_exclusions(&hard)
            };
            return Err(Rejected::new(
                &exclusions,
                ScheduleError::PlacementConstraintsUnsatisfied { excluded },
            ));
        }
        let slots = usize::from(include_local) + eligible.len();
        if spread && !shortfall.is_empty() && (slots as u32) < replicas {
            return Err(Rejected::new(
                &exclusions,
                ScheduleError::InsufficientCompatibleNodes {
                    replicas,
                    available: slots,
                    cause: shortfall_cause(&shortfall).to_string(),
                    excluded: join_exclusions(&shortfall),
                },
            ));
        }
        if eligible.is_empty() && !include_local {
            if !self.config.local_workloads_enabled {
                return Err(Rejected::new(
                    &exclusions,
                    ScheduleError::LocalWorkloadsDisabled { replicas },
                ));
            }
            if !hard.is_empty()
                && hard
                    .iter()
                    .all(|e| matches!(e.reason, ExclusionReason::DriverUnsupported { .. }))
            {
                return Err(Rejected::new(
                    &exclusions,
                    ScheduleError::NoIsolationProvider {
                        isolation: isolation_name(request.isolation).to_string(),
                        excluded: join_exclusions(&hard),
                    },
                ));
            }
            return Err(Rejected::new(
                &exclusions,
                ScheduleError::NoCompatibleNode {
                    image_platforms: request.image_platforms.join(", "),
                    local_platform: self
                        .config
                        .local_platform
                        .clone()
                        .unwrap_or_else(|| "unknown".to_string()),
                },
            ));
        }

        let full_pool = || -> Vec<PoolEntry> {
            control_index
                .filter(|_| include_local)
                .map(|index| PoolEntry {
                    node: index,
                    // `NodeScheduler` has no capacity report for the control
                    // plane; one the inventory supplies is used.
                    load: nodes[index].load_score(),
                })
                .into_iter()
                .chain(eligible.iter().map(|index| PoolEntry {
                    node: *index,
                    load: nodes[*index].load_score(),
                }))
                .collect()
        };
        let pool = {
            let under: Vec<PoolEntry> = full_pool()
                .into_iter()
                .filter(|entry| {
                    entry
                        .load
                        .is_none_or(|load| load < self.config.max_load_threshold)
                })
                .collect();
            if under.is_empty() {
                full_pool()
            } else {
                under
            }
        };
        let pool = if spread && !request.exclude_node_ids.is_empty() {
            let filtered: Vec<PoolEntry> = pool
                .into_iter()
                .filter(|entry| {
                    nodes[entry.node].is_control_plane()
                        || !request.exclude_node_ids.contains(&nodes[entry.node].id)
                })
                .collect();
            if filtered.is_empty() {
                full_pool()
            } else {
                filtered
            }
        } else {
            pool
        };

        let mut room: Vec<Option<Headroom>> = pool
            .iter()
            .map(|entry| headroom.get(&entry.node).copied())
            .collect();
        let chosen = match request.kind {
            WorkloadKind::Sandbox => self.rank_fewest_live(nodes, &pool, replicas, need, &mut room),
            WorkloadKind::Job if !request.affinity_nodes.is_empty() => self.rank_affinity(
                nodes,
                &pool,
                replicas,
                need,
                &mut room,
                request.affinity_nodes,
                spread,
            ),
            _ => self.rank_load(&pool, replicas, need, &mut room, spread),
        };
        let placed = chosen.len() as u32;
        if placed < replicas {
            return Err(Rejected::new(
                &exclusions,
                ScheduleError::InsufficientCapacity {
                    replicas,
                    placed,
                    needed: need,
                    excluded: "the enforcing nodes in the pool filled up while placing replicas"
                        .to_string(),
                },
            ));
        }
        let node_ids: Vec<i32> = chosen.iter().map(|i| nodes[pool[*i].node].id).collect();
        let warnings = spread_warning(&node_ids, spread).into_iter().collect();
        Ok(Placement {
            nodes: node_ids,
            exclusions,
            warnings,
            socket_required,
            reservation: need,
            reservation_assumed: assumed,
        })
    }

    /// Least loaded first (`NodeScheduler`'s `LeastLoaded`), round-robin
    /// when no node reports its load. Returns pool indexes; fewer than
    /// `replicas` when enforcing nodes fill up.
    fn rank_load(
        &self,
        pool: &[PoolEntry],
        replicas: u32,
        need: Reservation,
        room: &mut [Option<Headroom>],
        spread: bool,
    ) -> Vec<usize> {
        let fits = |room: &[Option<Headroom>], i: usize| room[i].is_none_or(|r| r.fits(need));
        let mut chosen = Vec::with_capacity(replicas as usize);
        let has_load = pool.iter().any(|entry| entry.load.is_some());
        let bump = if pool.is_empty() {
            0.0
        } else {
            1.0 / pool.len() as f64
        };
        let mut scores: Vec<f64> = pool.iter().map(|e| e.load.unwrap_or(0.5)).collect();
        if spread && pool.len() > 1 {
            let mut used = vec![false; pool.len()];
            let mut used_count = 0;
            for replica in 0..replicas as usize {
                if used_count >= pool.len() {
                    used.fill(false);
                    used_count = 0;
                }
                let pick = if has_load {
                    scores
                        .iter()
                        .enumerate()
                        .filter(|(i, _)| !used[*i] && fits(room, *i))
                        .min_by(|(_, a), (_, b)| {
                            a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)
                        })
                        .map(|(i, _)| i)
                        .or_else(|| {
                            // Every unused node is full: any node with room.
                            (0..pool.len()).find(|i| fits(room, *i))
                        })
                } else {
                    let start = replica % pool.len();
                    (0..pool.len())
                        .map(|offset| (start + offset) % pool.len())
                        .find(|i| fits(room, *i))
                };
                let Some(i) = pick else { break };
                chosen.push(i);
                if !used[i] {
                    used[i] = true;
                    used_count += 1;
                }
                scores[i] += bump;
                if let Some(r) = room[i].as_mut() {
                    r.take(need);
                }
            }
            return chosen;
        }
        for _ in 0..replicas {
            let pick = if has_load {
                scores
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| fits(room, *i))
                    .min_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
                    .map(|(i, _)| i)
            } else {
                let start = self.cursor.fetch_add(1, Ordering::Relaxed) % pool.len().max(1);
                (0..pool.len())
                    .map(|offset| (start + offset) % pool.len())
                    .find(|i| fits(room, *i))
            };
            let Some(i) = pick else { break };
            chosen.push(i);
            scores[i] += bump;
            if let Some(r) = room[i].as_mut() {
                r.take(need);
            }
        }
        chosen
    }

    /// Sandboxes: fewest live instances first, then least loaded.
    fn rank_fewest_live(
        &self,
        nodes: &[NodeFacts],
        pool: &[PoolEntry],
        replicas: u32,
        need: Reservation,
        room: &mut [Option<Headroom>],
    ) -> Vec<usize> {
        let mut live: Vec<u32> = pool.iter().map(|e| nodes[e.node].live_instances).collect();
        let mut chosen = Vec::new();
        for _ in 0..replicas {
            let pick = (0..pool.len())
                .filter(|i| room[*i].is_none_or(|r| r.fits(need)))
                .min_by(|a, b| {
                    live[*a].cmp(&live[*b]).then_with(|| {
                        let la = pool[*a].load.unwrap_or(0.5);
                        let lb = pool[*b].load.unwrap_or(0.5);
                        la.partial_cmp(&lb).unwrap_or(std::cmp::Ordering::Equal)
                    })
                });
            let Some(i) = pick else { break };
            chosen.push(i);
            live[i] += 1;
            if let Some(r) = room[i].as_mut() {
                r.take(need);
            }
        }
        chosen
    }

    /// Jobs: the owner's node(s) first, then by load.
    #[allow(clippy::too_many_arguments)]
    fn rank_affinity(
        &self,
        nodes: &[NodeFacts],
        pool: &[PoolEntry],
        replicas: u32,
        need: Reservation,
        room: &mut [Option<Headroom>],
        affinity: &[i32],
        spread: bool,
    ) -> Vec<usize> {
        let mut chosen = Vec::new();
        for _ in 0..replicas {
            let preferred = (0..pool.len()).find(|i| {
                affinity.contains(&nodes[pool[*i].node].id)
                    && room[*i].is_none_or(|r| r.fits(need))
                    && !(spread && chosen.contains(i))
            });
            match preferred {
                Some(i) => {
                    chosen.push(i);
                    if let Some(r) = room[i].as_mut() {
                        r.take(need);
                    }
                }
                None => {
                    let rest = self.rank_load(pool, 1, need, room, false);
                    let Some(i) = rest.first().copied() else {
                        break;
                    };
                    chosen.push(i);
                }
            }
        }
        chosen
    }
}

/// What [`explain`] knows beyond the node facts.
#[derive(Debug, Clone, Default)]
pub struct ExplainContext {
    /// Nodes hosting a service the workload is linked to (its database).
    pub linked_service_nodes: BTreeMap<i32, Vec<String>>,
    /// Nodes that already hold the artifact.
    pub image_cached_on: BTreeSet<i32>,
}

/// One recommended node and why.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NodeRecommendation {
    pub node_id: i32,
    pub node_name: String,
    /// Higher is better: headroom (0-1), plus 0.5 for a linked service on
    /// the node and 0.25 for a cached image.
    pub score: f64,
    pub reasons: Vec<String>,
}

/// A scheduler dry run (`WorkloadPlane::explain`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Explanation {
    /// Eligible nodes, best first.
    pub recommended: Vec<NodeRecommendation>,
    pub excluded: Vec<NodeExclusion>,
    /// Where the scheduler would put each replica now.
    pub placement: Option<Vec<i32>>,
    /// Why it cannot place it, when it cannot.
    pub unschedulable: Option<String>,
    pub unschedulable_reason: Option<String>,
    pub warnings: Vec<String>,
}

/// Dry-run `request` and rank the eligible nodes with reasons.
pub fn explain(
    scheduler: &UnifiedScheduler,
    nodes: &[NodeFacts],
    request: &ScheduleRequest<'_>,
    context: &ExplainContext,
) -> Explanation {
    // A throwaway scheduler, so a dry run never moves the real cursor.
    let dry = UnifiedScheduler::new(scheduler.config.clone());
    let traced = dry.schedule_traced(nodes, request);
    // The exclusions are recomputed with a single replica and no spread, so
    // the list is the same whether or not the placement succeeds.
    let single = PlacementSpec {
        replicas: 1,
        spread: false,
        ..request.placement.clone()
    };
    let probe = ScheduleRequest {
        placement: &single,
        ..request.clone()
    };
    let probe_outcome = dry.schedule_traced(nodes, &probe);
    let excluded = match (&traced, &probe_outcome) {
        (Ok(placement), _) => placement.exclusions.clone(),
        (Err(_), Ok(placement)) => placement.exclusions.clone(),
        // Neither places it: the probe's exclusions still say why per node.
        (Err(_), Err(rejected)) => rejected.exclusions.clone(),
    };
    let outcome = traced.map_err(|rejected| rejected.error);
    let excluded_ids: BTreeSet<i32> = excluded
        .iter()
        .filter(|e| e.excluded)
        .map(|e| e.node_id)
        .collect();
    let pinned = request.placement.pinned_nodes();
    let local_ok = scheduler.config.local_workloads_enabled
        && !request.exclude_control_plane
        && request.placement.label_selector().is_none()
        && pinned.is_none_or(|ids| ids.contains(&CONTROL_PLANE_NODE_ID));
    let mut recommended: Vec<NodeRecommendation> = nodes
        .iter()
        .filter(|node| !excluded_ids.contains(&node.id))
        .filter(|node| {
            if node.is_control_plane() {
                local_ok
            } else {
                pinned.is_none_or(|ids| ids.contains(&node.id))
                    && dry.first_failure(node, request, pinned).is_none()
            }
        })
        .map(|node| {
            let mut reasons = Vec::new();
            let mut score = 0.0;
            match node.load_score() {
                Some(load) => {
                    let headroom = (1.0 - load).clamp(0.0, 1.0);
                    score += headroom;
                    reasons.push(format!(
                        "{:.0}% headroom after system_reserved ({}m CPU / {} MB)",
                        headroom * 100.0,
                        node.system_reserved.cpu_millis,
                        node.system_reserved.memory_mb
                    ));
                }
                None => {
                    score += 0.5;
                    reasons.push("does not report its load; counted at 50%".to_string());
                }
            }
            if let Some(services) = context.linked_service_nodes.get(&node.id) {
                score += 0.5;
                reasons.push(format!("runs linked service(s): {}", services.join(", ")));
            }
            if context.image_cached_on.contains(&node.id) {
                score += 0.25;
                reasons.push("already has the image".to_string());
            }
            if !request.image_platforms.is_empty() {
                match &node.architecture {
                    Some(platform) => reasons.push(format!("runs {platform}, which has an image")),
                    None if node.is_control_plane() => {}
                    None => reasons.push("architecture not reported; assumed compatible".into()),
                }
            }
            if node.resource_policy == NodeResourcePolicy::Enforced {
                reasons.push("enforces reservations".to_string());
            }
            NodeRecommendation {
                node_id: node.id,
                node_name: node.name.clone(),
                score,
                reasons,
            }
        })
        .collect();
    recommended.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.node_id.cmp(&b.node_id))
    });
    match outcome {
        Ok(placement) => Explanation {
            recommended,
            excluded,
            placement: Some(placement.nodes),
            unschedulable: None,
            unschedulable_reason: None,
            warnings: placement.warnings,
        },
        Err(error) => Explanation {
            recommended,
            excluded,
            placement: None,
            unschedulable_reason: Some(error.reason().to_string()),
            unschedulable: Some(error.to_string()),
            warnings: Vec::new(),
        },
    }
}

/// A suggested `target_nodes` for an environment.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PinningRecommendation {
    /// Nodes to pin, best first; empty when pinning would not help.
    pub nodes: Vec<i32>,
    pub reasons: Vec<String>,
    /// The environment's current pins.
    pub current: Vec<i32>,
    /// Whether applying it changes anything.
    pub changes_placement: bool,
}

/// Recommend pins from where the environment ran recently (`history`: node
/// id to the number of its containers there, over the window the caller
/// chose) and the explain ranking. Only nodes the scheduler finds eligible
/// are recommended, so the result never pins an environment to a node it
/// could not run on.
pub fn recommend_pinning(
    explanation: &Explanation,
    history: &BTreeMap<i32, u64>,
    context: &ExplainContext,
    replicas: u32,
    current: &[i32],
) -> PinningRecommendation {
    let mut candidates: Vec<&NodeRecommendation> = explanation.recommended.iter().collect();
    let weight = |node: &NodeRecommendation| -> (bool, u64) {
        (
            context.linked_service_nodes.contains_key(&node.node_id),
            history.get(&node.node_id).copied().unwrap_or(0),
        )
    };
    candidates.sort_by(|a, b| {
        weight(b).cmp(&weight(a)).then(
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal),
        )
    });
    let wanted = replicas.max(1) as usize;
    let chosen: Vec<&NodeRecommendation> = candidates.into_iter().take(wanted).collect();
    let mut reasons = Vec::new();
    for node in &chosen {
        let mut why = Vec::new();
        if let Some(services) = context.linked_service_nodes.get(&node.node_id) {
            why.push(format!("hosts linked service(s) {}", services.join(", ")));
        }
        if let Some(count) = history.get(&node.node_id) {
            why.push(format!(
                "ran {count} of this environment's recent containers"
            ));
        }
        if why.is_empty() {
            why.push(format!("eligible with score {:.2}", node.score));
        }
        reasons.push(format!(
            "node {} ({}): {}",
            node.node_id,
            node.node_name,
            why.join("; ")
        ));
    }
    let nodes: Vec<i32> = chosen.iter().map(|n| n.node_id).collect();
    let mut sorted_current = current.to_vec();
    sorted_current.sort_unstable();
    let mut sorted_new = nodes.clone();
    sorted_new.sort_unstable();
    if nodes.is_empty() {
        reasons.push(
            "no eligible node: pinning cannot help until a node passes the filters".to_string(),
        );
    }
    PinningRecommendation {
        changes_placement: !nodes.is_empty() && sorted_new != sorted_current,
        nodes,
        reasons,
        current: current.to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use temps_workload::{LabelValues, ResourceAmounts};

    fn worker(id: i32, name: &str) -> NodeFacts {
        NodeFacts {
            id,
            name: name.to_string(),
            status: "active".to_string(),
            heartbeat_fresh: true,
            role: NodeRole::Ordinary,
            labels: BTreeMap::new(),
            architecture: Some("linux/amd64".to_string()),
            isolations: vec![Isolation::Process],
            grants_socket: false,
            usage: None,
            resource_policy: NodeResourcePolicy::BestEffort,
            system_reserved: SystemReserved::NONE,
            committed: Reservation::default(),
            in_flight: Reservation::default(),
            live_instances: 0,
        }
    }

    fn loaded(mut node: NodeFacts, cpu: f64, mem_used_gb: f64, mem_total_gb: f64) -> NodeFacts {
        node.usage = Some(NodeUsage {
            cpu_percent: Some(cpu),
            memory_used_bytes: Some(mem_used_gb * 1024.0 * BYTES_PER_MB),
            memory_total_bytes: Some(mem_total_gb * 1024.0 * BYTES_PER_MB),
            cpu_cores: Some(2.0),
        });
        node
    }

    fn control() -> NodeFacts {
        let mut node = NodeFacts::control_plane(None);
        node.system_reserved = SystemReserved::NONE;
        node
    }

    fn scheduler() -> UnifiedScheduler {
        UnifiedScheduler::new(SchedulerConfig::new(platforms_equal))
    }

    fn request<'a>(placement: &'a PlacementSpec, resources: &'a Resources) -> ScheduleRequest<'a> {
        ScheduleRequest {
            kind: WorkloadKind::Deployment,
            placement,
            resources,
            isolation: Isolation::Process,
            image_platforms: &[],
            socket_gate: None,
            exclude_node_ids: &[],
            exclude_control_plane: false,
            affinity_nodes: &[],
        }
    }

    fn reasons(placement: &Placement) -> Vec<(i32, &'static str)> {
        placement
            .exclusions
            .iter()
            .map(|e| (e.node_id, e.reason.code()))
            .collect()
    }

    #[test]
    fn filters_run_in_order_and_keep_the_first_reason() {
        let mut draining = worker(1, "a-draining");
        draining.status = "draining".into();
        let mut offline = worker(2, "b-offline");
        offline.status = "offline".into();
        let mut builder = worker(3, "c-builder");
        builder.role = NodeRole::Builder;
        let mut dedicated = worker(4, "d-dedicated");
        dedicated.role = NodeRole::Dedicated;
        let mut arm = worker(5, "e-arm");
        arm.architecture = Some("linux/arm64".into());
        let mut vm_only = worker(6, "f-vm");
        vm_only.isolations = vec![Isolation::MicroVm];
        let ok = worker(7, "g-ok");
        let nodes = vec![
            control(),
            draining,
            offline,
            builder,
            dedicated,
            arm,
            vm_only,
            ok,
        ];
        let placement = PlacementSpec::default();
        let resources = Resources::default();
        let platforms = vec!["linux/amd64".to_string()];
        let mut req = request(&placement, &resources);
        req.image_platforms = &platforms;
        let outcome = scheduler().schedule(&nodes, &req).expect("placed");
        assert_eq!(
            reasons(&outcome),
            vec![
                (1, "NodeDraining"),
                (2, "NodeUnavailable"),
                (3, "BuildOnlyNode"),
                (4, "DedicatedNode"),
                (5, "IncompatibleArchitecture"),
                (6, "DriverUnsupported"),
            ]
        );
        assert_eq!(outcome.nodes.len(), 1);
        assert!([0, 7].contains(&outcome.nodes[0]));
    }

    #[test]
    fn the_reserve_is_deducted_before_the_load_threshold() {
        // 2 cores at 80% CPU, 3 of 4 GB used: 0.6*80 + 0.4*75 = 78% without
        // a reserve; with 0.5 vCPU / 1 GB reserved, 0.6*106.7 + 0.4*100 =
        // 104%: over the threshold.
        let busy = loaded(worker(1, "busy"), 80.0, 3.0, 4.0);
        let idle = loaded(worker(2, "idle"), 85.0, 3.2, 4.0);
        assert!((busy.load_score().unwrap() - 0.78).abs() < 1e-9);
        let mut reserved = busy.clone();
        reserved.system_reserved = SystemReserved {
            cpu_millis: 500,
            memory_mb: 1024,
        };
        let with_reserve = reserved.load_score().unwrap();
        assert!(with_reserve > 1.0, "{with_reserve}");
        assert_eq!(
            reserved.schedulable_memory_mb(),
            Some(3.0 * 1024.0),
            "4 GB minus the 1 GB reserve"
        );
        assert_eq!(reserved.schedulable_cpu_millis(), Some(1500.0));

        // Without reserves the busy node is the least loaded and wins; with
        // its reserve it is over the threshold and the other node wins.
        let placement = PlacementSpec::default();
        let resources = Resources::default();
        let mut cfg = SchedulerConfig::new(platforms_equal);
        cfg.local_workloads_enabled = false;
        let plain = UnifiedScheduler::new(cfg.clone())
            .schedule(
                &[busy.clone(), idle.clone()],
                &request(&placement, &resources),
            )
            .expect("placed");
        assert_eq!(plain.nodes, vec![1]);
        let deducted = UnifiedScheduler::new(cfg)
            .schedule(&[reserved, idle], &request(&placement, &resources))
            .expect("placed");
        assert_eq!(deducted.nodes, vec![2]);
    }

    #[test]
    fn an_enforcing_node_refuses_what_exceeds_its_schedulable_capacity() {
        let mut node = loaded(worker(1, "db-host"), 10.0, 1.0, 4.0);
        node.resource_policy = NodeResourcePolicy::Enforced;
        node.system_reserved = SystemReserved::WORKER;
        // 2000m - 250m reserve - 1500m committed = 250m left.
        node.committed = Reservation {
            cpu_millis: 1500,
            memory_mb: 1024,
        };
        let mut cfg = SchedulerConfig::new(platforms_equal);
        cfg.local_workloads_enabled = false;
        let placement = PlacementSpec {
            replicas: 2,
            ..Default::default()
        };
        let resources = Resources {
            requests: Some(ResourceAmounts {
                cpu_millis: Some(200),
                memory_mb: Some(256),
                ..Default::default()
            }),
            limits: None,
        };
        let error = UnifiedScheduler::new(cfg.clone())
            .schedule(
                std::slice::from_ref(&node),
                &request(&placement, &resources),
            )
            .expect_err("only one replica fits");
        assert!(
            matches!(
                error,
                ScheduleError::InsufficientCapacity {
                    placed: 1,
                    replicas: 2,
                    ..
                }
            ),
            "{error}"
        );
        assert_eq!(error.reason(), "InsufficientCapacity");

        // In-flight placements count too: one already in flight fills it.
        node.in_flight = Reservation {
            cpu_millis: 250,
            memory_mb: 512,
        };
        let one = PlacementSpec::default();
        let error = UnifiedScheduler::new(cfg.clone())
            .schedule(std::slice::from_ref(&node), &request(&one, &resources))
            .expect_err("full");
        assert!(
            error.to_string().contains("enforces reservations"),
            "{error}"
        );

        // The same node in best_effort takes it (load-based placement).
        node.resource_policy = NodeResourcePolicy::BestEffort;
        let placed = UnifiedScheduler::new(cfg)
            .schedule(
                std::slice::from_ref(&node),
                &request(&placement, &resources),
            )
            .expect("best effort");
        assert_eq!(placed.nodes, vec![1, 1]);
    }

    #[test]
    fn an_enforcing_node_without_a_core_count_is_not_guessed() {
        let mut node = worker(1, "old-agent");
        node.resource_policy = NodeResourcePolicy::Enforced;
        node.usage = Some(NodeUsage {
            cpu_percent: Some(5.0),
            memory_used_bytes: Some(1.0),
            memory_total_bytes: Some(8.0 * 1024.0 * BYTES_PER_MB),
            cpu_cores: None,
        });
        let mut cfg = SchedulerConfig::new(platforms_equal);
        cfg.local_workloads_enabled = false;
        let placement = PlacementSpec::default();
        let resources = Resources::default();
        let error = UnifiedScheduler::new(cfg)
            .schedule(&[node], &request(&placement, &resources))
            .expect_err("capacity unknown");
        assert!(error.to_string().contains("core count"), "{error}");
    }

    #[test]
    fn pinning_is_strict_and_names_why() {
        let mut offline = worker(2, "pinned-offline");
        offline.status = "offline".into();
        let other = worker(3, "free");
        let nodes = vec![control(), offline, other];
        let placement = PlacementSpec {
            nodes: Some(vec![2]),
            ..Default::default()
        };
        let resources = Resources::default();
        let error = scheduler()
            .schedule(&nodes, &request(&placement, &resources))
            .expect_err("never moved elsewhere");
        assert_eq!(error.reason(), "PlacementConstraintsUnsatisfied");
        assert!(error.to_string().contains("pinned-offline"), "{error}");

        // A pin to a node that does not exist says so.
        let missing = PlacementSpec {
            nodes: Some(vec![42]),
            ..Default::default()
        };
        let error = scheduler()
            .schedule(&nodes, &request(&missing, &resources))
            .expect_err("missing");
        assert!(error.to_string().contains("node 42"), "{error}");

        // Pinned to the control plane with local workloads disabled.
        let mut cfg = SchedulerConfig::new(platforms_equal);
        cfg.local_workloads_enabled = false;
        let local = PlacementSpec {
            nodes: Some(vec![0]),
            ..Default::default()
        };
        let error = UnifiedScheduler::new(cfg)
            .schedule(&nodes, &request(&local, &resources))
            .expect_err("disabled");
        assert_eq!(error.reason(), "LocalWorkloadsDisabled");
    }

    #[test]
    fn labels_select_with_or_values_and_exclude_the_control_plane() {
        let mut us = worker(1, "us");
        us.labels.insert("region".into(), "us".into());
        let mut eu = worker(2, "eu");
        eu.labels.insert("region".into(), "eu".into());
        let mut asia = worker(3, "asia");
        asia.labels.insert("region".into(), "asia".into());
        let nodes = vec![control(), us, eu, asia];
        let placement = PlacementSpec {
            labels: Some(BTreeMap::from([(
                "region".to_string(),
                LabelValues::AnyOf(vec!["us".into(), "asia".into()]),
            )])),
            replicas: 2,
            spread: true,
            ..Default::default()
        };
        let resources = Resources::default();
        let placed = scheduler()
            .schedule(&nodes, &request(&placement, &resources))
            .expect("placed");
        assert_eq!(placed.nodes, vec![1, 3]);
    }

    #[test]
    fn spread_ranks_sandboxes_by_live_instances_and_jobs_by_affinity() {
        let mut busy = worker(1, "a");
        busy.live_instances = 4;
        let quiet = worker(2, "b");
        let nodes = vec![busy, quiet];
        let placement = PlacementSpec::default();
        let resources = Resources::default();
        let mut cfg = SchedulerConfig::new(platforms_equal);
        cfg.local_workloads_enabled = false;
        let s = UnifiedScheduler::new(cfg);
        let mut sandbox = request(&placement, &resources);
        sandbox.kind = WorkloadKind::Sandbox;
        assert_eq!(s.schedule(&nodes, &sandbox).unwrap().nodes, vec![2]);
        let mut job = request(&placement, &resources);
        job.kind = WorkloadKind::Job;
        job.affinity_nodes = &[1];
        assert_eq!(s.schedule(&nodes, &job).unwrap().nodes, vec![1]);
    }

    #[test]
    fn the_socket_gate_needs_a_granting_host() {
        let w = worker(1, "w");
        let nodes = vec![control(), w];
        let placement = PlacementSpec::default();
        let resources = Resources::default();
        let mut req = request(&placement, &resources);
        req.socket_gate = Some(SocketGate {
            project_slug: "infra-agent".into(),
            control_plane_grants: false,
        });
        let error = scheduler().schedule(&nodes, &req).expect_err("no grant");
        assert_eq!(error.reason(), "DockerSocketNotSchedulable");
        req.socket_gate = Some(SocketGate {
            project_slug: "infra-agent".into(),
            control_plane_grants: true,
        });
        let placed = scheduler().schedule(&nodes, &req).expect("local grants");
        assert_eq!(placed.nodes, vec![0]);
        assert!(placed.socket_required);
    }

    #[test]
    fn explain_ranks_with_reasons() {
        let near_db = loaded(worker(1, "near-db"), 50.0, 2.0, 4.0);
        let idle = loaded(worker(2, "idle"), 5.0, 0.5, 4.0);
        let mut arm = worker(3, "arm");
        arm.architecture = Some("linux/arm64".into());
        let nodes = vec![control(), near_db, idle, arm];
        let placement = PlacementSpec::default();
        let resources = Resources::default();
        let platforms = vec!["linux/amd64".to_string()];
        let mut req = request(&placement, &resources);
        req.image_platforms = &platforms;
        let context = ExplainContext {
            linked_service_nodes: BTreeMap::from([(1, vec!["postgres-main".to_string()])]),
            image_cached_on: BTreeSet::from([1]),
        };
        let explanation = explain(&scheduler(), &nodes, &req, &context);
        let first = &explanation.recommended[0];
        assert_eq!(first.node_id, 1, "{explanation:?}");
        assert!(first.reasons.iter().any(|r| r.contains("postgres-main")));
        assert!(first
            .reasons
            .iter()
            .any(|r| r.contains("already has the image")));
        assert!(first.reasons.iter().any(|r| r.contains("headroom")));
        assert!(first.reasons.iter().any(|r| r.contains("linux/amd64")));
        assert!(explanation.recommended.iter().all(|r| r.node_id != 3));
        assert!(explanation
            .excluded
            .iter()
            .any(|e| e.node_id == 3 && e.reason.code() == "IncompatibleArchitecture"));
        assert!(explanation.placement.is_some());

        let pin = recommend_pinning(&explanation, &BTreeMap::from([(2, 9)]), &context, 1, &[]);
        assert_eq!(pin.nodes, vec![1], "{pin:?}");
        assert!(pin.changes_placement);
        assert!(pin.reasons[0].contains("postgres-main"));
    }

    #[test]
    fn explain_reports_why_nothing_fits() {
        let mut offline = worker(1, "gone");
        offline.status = "offline".into();
        let placement = PlacementSpec {
            nodes: Some(vec![1]),
            ..Default::default()
        };
        let resources = Resources::default();
        let explanation = explain(
            &scheduler(),
            &[control(), offline],
            &request(&placement, &resources),
            &ExplainContext::default(),
        );
        assert!(explanation.placement.is_none());
        assert_eq!(
            explanation.unschedulable_reason.as_deref(),
            Some("PlacementConstraintsUnsatisfied")
        );
        assert!(explanation.recommended.is_empty());
    }

    #[test]
    fn a_reservation_prefers_requests_then_limits_then_the_kind_default() {
        let (default, assumed) = reservation_of(WorkloadKind::Deployment, &Resources::default());
        assert!(assumed);
        assert_eq!(default, kind_default_reservation(WorkloadKind::Deployment));
        let resources = Resources {
            requests: Some(ResourceAmounts {
                cpu_millis: Some(100),
                ..Default::default()
            }),
            limits: Some(ResourceAmounts {
                cpu_millis: Some(900),
                memory_mb: Some(300),
                ..Default::default()
            }),
        };
        assert_eq!(
            reservation_of(WorkloadKind::Deployment, &resources),
            (
                Reservation {
                    cpu_millis: 100,
                    memory_mb: 300
                },
                false
            )
        );
    }

    #[test]
    fn heartbeat_capacity_is_read_from_the_agent_json() {
        let usage = NodeUsage::from_capacity_json(&serde_json::json!({
            "cpu_percent": 12.5, "memory_used_bytes": 10, "memory_total_bytes": 20,
            "cpu_cores": 4
        }))
        .unwrap();
        assert_eq!(usage.cpu_cores, Some(4.0));
        assert_eq!(NodeUsage::from_capacity_json(&serde_json::json!({})), None);
    }

    /// Firecracker for deployments (ADR-055 Phase 10): a node that reports
    /// nothing runs Docker, so it never takes a microVM request.
    #[test]
    fn unreported_isolations_mean_process_only() {
        let mut silent = worker(1, "a-silent");
        silent.isolations = Vec::new();
        assert!(silent.provides(Isolation::Process));
        assert!(!silent.provides(Isolation::MicroVm));
        assert!(!silent.provides(Isolation::InHost));
    }

    #[test]
    fn micro_vm_request_lands_on_a_node_that_offers_it() {
        let mut silent = worker(1, "a-silent");
        silent.isolations = Vec::new();
        let mut vm = worker(2, "b-vm");
        vm.isolations = vec![Isolation::Process, Isolation::MicroVm];
        let nodes = vec![control(), silent, vm];
        let placement = PlacementSpec::default();
        let resources = Resources::default();
        let mut req = request(&placement, &resources);
        req.isolation = Isolation::MicroVm;
        let outcome = scheduler().schedule(&nodes, &req).expect("placed");
        assert_eq!(outcome.nodes, vec![2]);
        let mut excluded = reasons(&outcome);
        excluded.sort();
        assert_eq!(
            excluded,
            vec![(0, "DriverUnsupported"), (1, "DriverUnsupported")]
        );
    }

    #[test]
    fn micro_vm_request_with_no_provider_is_refused_with_the_reason() {
        let nodes = vec![control(), worker(1, "a-docker")];
        let placement = PlacementSpec::default();
        let resources = Resources::default();
        let mut req = request(&placement, &resources);
        req.isolation = Isolation::MicroVm;
        let error = scheduler().schedule(&nodes, &req).unwrap_err();
        let message = error.to_string();
        assert!(
            message.contains("has no runtime that provides micro_vm isolation"),
            "{message}"
        );
        assert_eq!(error.reason(), "NoIsolationProvider");

        // `explain` names the same reason for every node.
        let explanation = explain(&scheduler(), &nodes, &req, &ExplainContext::default());
        assert!(explanation.placement.is_none());
        assert!(explanation.unschedulable.is_some());
        let mut codes: Vec<(i32, &str)> = explanation
            .excluded
            .iter()
            .map(|e| (e.node_id, e.reason.code()))
            .collect();
        codes.sort();
        assert_eq!(
            codes,
            vec![(0, "DriverUnsupported"), (1, "DriverUnsupported")]
        );
    }

    #[test]
    fn control_plane_with_a_micro_vm_driver_takes_the_request() {
        let mut control = control();
        control.isolations = vec![Isolation::Process, Isolation::MicroVm];
        let nodes = vec![control, worker(1, "a-docker")];
        let placement = PlacementSpec {
            nodes: Some(vec![CONTROL_PLANE_NODE_ID]),
            ..PlacementSpec::default()
        };
        let resources = Resources::default();
        let mut req = request(&placement, &resources);
        req.isolation = Isolation::MicroVm;
        let outcome = scheduler().schedule(&nodes, &req).expect("placed");
        assert_eq!(outcome.nodes, vec![CONTROL_PLANE_NODE_ID]);
    }
}
