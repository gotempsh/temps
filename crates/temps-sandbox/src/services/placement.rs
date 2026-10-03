// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Sandbox placement (ADR-048): which node a new sandbox runs on.
//!
//! * The operator allow-list lives in `AppSettings.agent_sandbox.allowed_node_ids`.
//!   `None` = every node (the default); the control plane is id `0`.
//! * An explicit request (`node: "worker-1"`, `"7"`, `"control-plane"`) must
//!   name an allowed, active node — otherwise the create fails with a typed
//!   error. There is never a silent fallback to another node.
//! * Without a request, the control plane is used when it is allowed, so a
//!   single-node install behaves exactly as before. When the operator has
//!   excluded it, the allowed active worker with the fewest live sandboxes
//!   wins (ties → lowest id).

use sea_orm::{
    ColumnTrait, DatabaseConnection, EntityTrait, FromQueryResult, QueryFilter, QueryOrder,
    QuerySelect,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use temps_entities::{nodes, sandboxes};
use utoipa::ToSchema;

use crate::error::SandboxError;

/// Id the placement API uses for the control plane (it has no `nodes` row).
/// Matches `CONTROL_PLANE_NODE_ID` used for services.
pub const CONTROL_PLANE_NODE_ID: i32 = 0;
/// Name the placement API and responses use for the control plane.
pub const CONTROL_PLANE_NAME: &str = "control-plane";

/// Only `active` nodes accept new sandboxes. Draining/drained nodes keep the
/// sandboxes they host but get no new ones.
const PLACEABLE_STATUS: &str = "active";

/// Operator allow-list. `None` = all nodes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlacementPolicy {
    pub allowed_node_ids: Option<Vec<i32>>,
}

impl PlacementPolicy {
    pub fn allows(&self, node_id: i32) -> bool {
        self.allowed_node_ids
            .as_ref()
            .is_none_or(|ids| ids.contains(&node_id))
    }
}

/// How a caller named a node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestedNode {
    ControlPlane,
    Id(i32),
    Name(String),
}

impl RequestedNode {
    pub fn parse(raw: &str) -> Self {
        let value = raw.trim();
        match value {
            "0" | CONTROL_PLANE_NAME | "control_plane" | "local" => Self::ControlPlane,
            _ => match value.parse::<i32>() {
                Ok(id) if id > 0 => Self::Id(id),
                _ => Self::Name(value.to_string()),
            },
        }
    }
}

/// `allowed` without ids of nodes that no longer exist. The control plane
/// (`0`) always exists.
pub fn existing_node_ids(allowed: Option<Vec<i32>>, workers: &[WorkerNode]) -> Option<Vec<i32>> {
    allowed.map(|ids| {
        ids.into_iter()
            .filter(|id| *id == CONTROL_PLANE_NODE_ID || workers.iter().any(|w| w.id == *id))
            .collect()
    })
}

/// Pick the `requested` node out of a [`describe`] listing.
pub fn find_node(nodes: Vec<PlacementNode>, requested: &RequestedNode) -> Option<PlacementNode> {
    nodes.into_iter().find(|n| match requested {
        RequestedNode::ControlPlane => n.is_control_plane,
        RequestedNode::Id(id) => !n.is_control_plane && n.id == *id,
        RequestedNode::Name(name) => !n.is_control_plane && n.name == *name,
    })
}

/// One row of `GET /sandboxes/placement`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PlacementNode {
    /// Node id; `0` for the control plane.
    pub id: i32,
    pub name: String,
    pub is_control_plane: bool,
    /// Node status (`active`, `offline`, `draining`, …). Always `active`
    /// for the control plane.
    pub status: String,
    /// Allowed by the operator's sandbox placement settings.
    pub allowed: bool,
    /// Can take a new sandbox right now (allowed and active).
    pub eligible: bool,
    /// Why the node is not eligible, when it isn't.
    pub reason: Option<String>,
    /// Sandboxes currently hosted on the node (not destroyed).
    pub live_sandboxes: u64,
}

/// Why an otherwise active worker cannot take new sandboxes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Blocked {
    /// A build-only node (`temps.sh/role=builder`).
    BuildOnly,
    /// No heartbeat within [`HEARTBEAT_THRESHOLD_SECS`].
    NoHeartbeat,
    /// The node's agent address is plain `http://`. A sandbox call carries
    /// the node token, environment variables and file contents, so it is
    /// only made over an mTLS (`https://`) node address.
    PlainHttp,
    /// An eviction of this node's sandboxes is running.
    Evicting,
}

impl Blocked {
    /// Phrased to follow "Node 'x' is …" (see [`SandboxError::NodeNotReady`]).
    pub fn status_phrase(&self) -> &'static str {
        match self {
            Blocked::BuildOnly => "a build-only node",
            Blocked::NoHeartbeat => "not sending heartbeats",
            Blocked::PlainHttp => {
                "reachable only over a plain http:// address (sandboxes need an https (mTLS) \
                 node address; re-join the node with `temps join` to give it one)"
            }
            Blocked::Evicting => "having its sandboxes evicted",
        }
    }

    /// The reason shown in the placement listing.
    pub fn reason(&self) -> String {
        match self {
            Blocked::PlainHttp => {
                "node address uses http://; sandboxes need an https (mTLS) node address".to_string()
            }
            Blocked::Evicting => "sandboxes on this node are being evicted".to_string(),
            Blocked::BuildOnly | Blocked::NoHeartbeat => {
                format!("node is {}", self.status_phrase())
            }
        }
    }
}

/// A worker as far as placement cares.
#[derive(Debug, Clone)]
pub struct WorkerNode {
    pub id: i32,
    pub name: String,
    pub status: String,
    /// Why an otherwise active node cannot take sandboxes.
    pub blocked: Option<Blocked>,
}

/// Why a worker cannot take a new sandbox right now.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Unplaceable<'a> {
    /// Its status is not `active` (offline, draining, …).
    Status(&'a str),
    Blocked(&'a Blocked),
}

impl Unplaceable<'_> {
    /// Phrased to follow "Node 'x' is …".
    fn status_phrase(&self) -> String {
        match self {
            Unplaceable::Status(status) => (*status).to_string(),
            Unplaceable::Blocked(blocked) => blocked.status_phrase().to_string(),
        }
    }

    fn reason(&self) -> String {
        match self {
            Unplaceable::Status(status) => format!("node is {status}"),
            Unplaceable::Blocked(blocked) => blocked.reason(),
        }
    }
}

impl WorkerNode {
    /// `Err` when the node cannot take a new sandbox right now.
    fn placeable(&self) -> Result<(), Unplaceable<'_>> {
        if self.status != PLACEABLE_STATUS {
            return Err(Unplaceable::Status(&self.status));
        }
        match &self.blocked {
            Some(blocked) => Err(Unplaceable::Blocked(blocked)),
            None => Ok(()),
        }
    }
}

/// Mark the workers whose sandboxes are being evicted: they take no new
/// sandboxes until the eviction finishes.
pub fn mark_evicting(workers: &mut [WorkerNode], evicting: &std::collections::HashSet<i32>) {
    for w in workers.iter_mut().filter(|w| evicting.contains(&w.id)) {
        w.blocked = Some(Blocked::Evicting);
    }
}

/// Whether a node agent address is mTLS (`https://`).
pub fn is_https_address(address: &str) -> bool {
    address
        .trim()
        .get(..8)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("https://"))
}

/// Node label marking a build-only node; same contract as the deploy
/// scheduler (`temps_deployments::services::node_scheduler`).
const NODE_ROLE_LABEL: &str = "temps.sh/role";
const BUILDER_NODE_ROLE: &str = "builder";

/// A node that has not sent a heartbeat for this long is treated as down,
/// matching the deploy scheduler's threshold.
const HEARTBEAT_THRESHOLD_SECS: i64 = 90;

/// Where placement wants a new sandbox, before any chosen worker is asked
/// whether it can run one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Candidates {
    ControlPlane,
    /// The worker the caller asked for by name or id.
    Requested {
        id: i32,
        name: String,
    },
    /// Allowed, placeable workers, best first (fewest live sandboxes, ties
    /// to the lowest id). Never empty.
    Ranked(Vec<(i32, String)>),
}

/// Pure placement decision, separated from the database for testing.
/// `live` maps node id → live sandbox count (control plane under id 0).
pub fn candidates(
    policy: &PlacementPolicy,
    workers: &[WorkerNode],
    live: &HashMap<i32, u64>,
    requested: Option<&RequestedNode>,
) -> Result<Candidates, SandboxError> {
    match requested {
        Some(RequestedNode::ControlPlane) => {
            if policy.allows(CONTROL_PLANE_NODE_ID) {
                Ok(Candidates::ControlPlane)
            } else {
                Err(SandboxError::NodeNotAllowed {
                    node: CONTROL_PLANE_NAME.to_string(),
                })
            }
        }
        Some(req) => {
            let node = workers
                .iter()
                .find(|w| match req {
                    RequestedNode::Id(id) => w.id == *id,
                    RequestedNode::Name(name) => w.name == *name,
                    RequestedNode::ControlPlane => false,
                })
                .ok_or_else(|| SandboxError::NodeNotFound {
                    node: match req {
                        RequestedNode::Id(id) => id.to_string(),
                        RequestedNode::Name(name) => name.clone(),
                        RequestedNode::ControlPlane => CONTROL_PLANE_NAME.to_string(),
                    },
                })?;
            if !policy.allows(node.id) {
                return Err(SandboxError::NodeNotAllowed {
                    node: node.name.clone(),
                });
            }
            if let Err(why) = node.placeable() {
                return Err(SandboxError::NodeNotReady {
                    node: node.name.clone(),
                    status: why.status_phrase(),
                });
            }
            Ok(Candidates::Requested {
                id: node.id,
                name: node.name.clone(),
            })
        }
        None => {
            if policy.allows(CONTROL_PLANE_NODE_ID) {
                return Ok(Candidates::ControlPlane);
            }
            let mut ranked: Vec<&WorkerNode> = workers
                .iter()
                .filter(|w| policy.allows(w.id) && w.placeable().is_ok())
                .collect();
            if ranked.is_empty() {
                return Err(SandboxError::NoPlacementNode);
            }
            ranked.sort_by_key(|w| (live.get(&w.id).copied().unwrap_or(0), w.id));
            Ok(Candidates::Ranked(
                ranked.into_iter().map(|w| (w.id, w.name.clone())).collect(),
            ))
        }
    }
}

/// [`candidates`] without asking any worker: the node placement would try
/// first (`None` = control plane).
pub fn choose(
    policy: &PlacementPolicy,
    workers: &[WorkerNode],
    live: &HashMap<i32, u64>,
    requested: Option<&RequestedNode>,
) -> Result<Option<i32>, SandboxError> {
    Ok(match candidates(policy, workers, live, requested)? {
        Candidates::ControlPlane => None,
        Candidates::Requested { id, .. } => Some(id),
        Candidates::Ranked(ranked) => ranked.first().map(|(id, _)| *id),
    })
}

/// Asks a worker whether it can run a sandbox right now.
#[async_trait::async_trait]
pub trait NodeProbe: Send + Sync {
    /// `Err(reason)` when it cannot (unreachable, Docker down, an agent
    /// that predates sandbox support). The reason is shown after the node's
    /// name, so it need not repeat it.
    async fn check(&self, node_id: i32) -> Result<(), String>;
}

/// Most workers one automatic placement asks before giving up: a cluster
/// of broken workers must not turn one create into a long series of
/// timeouts.
pub const MAX_PROBED_CANDIDATES: usize = 3;

/// How long one worker gets to answer whether it can run a sandbox.
pub const NODE_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Settle [`candidates`] into a node by asking the chosen worker(s) whether
/// they can run a sandbox. The control plane is never asked (single-node
/// installs make no node calls). An explicitly requested worker that cannot
/// run one is an error naming it; automatic placement moves on to the next
/// candidate, up to [`MAX_PROBED_CANDIDATES`].
pub async fn settle(
    candidates: Candidates,
    probe: &dyn NodeProbe,
) -> Result<Option<i32>, SandboxError> {
    match candidates {
        Candidates::ControlPlane => Ok(None),
        Candidates::Requested { id, name } => match probe.check(id).await {
            Ok(()) => Ok(Some(id)),
            Err(reason) => {
                tracing::warn!(
                    node_id = id,
                    node = %name,
                    %reason,
                    "sandbox placement: requested node cannot run sandboxes"
                );
                Err(SandboxError::NodeNotReady {
                    node: name,
                    status: format!("not ready to run sandboxes ({reason})"),
                })
            }
        },
        Candidates::Ranked(ranked) => {
            let mut failures = Vec::new();
            for (id, name) in ranked.into_iter().take(MAX_PROBED_CANDIDATES) {
                match probe.check(id).await {
                    Ok(()) => return Ok(Some(id)),
                    Err(reason) => {
                        tracing::warn!(
                            node_id = id,
                            node = %name,
                            %reason,
                            "sandbox placement: skipping a node that cannot run sandboxes"
                        );
                        failures.push(format!("node '{name}': {reason}"));
                    }
                }
            }
            Err(SandboxError::NoReadyPlacementNode {
                reasons: failures.join("; "),
            })
        }
    }
}

/// Reports every worker ready. For tests that don't exercise probing.
#[cfg(test)]
pub(crate) struct EveryNodeReady;

#[cfg(test)]
#[async_trait::async_trait]
impl NodeProbe for EveryNodeReady {
    async fn check(&self, _node_id: i32) -> Result<(), String> {
        Ok(())
    }
}

/// [`NodeProbe`] over the worker's agent API (`/agent/sandboxes/status`),
/// using the same resolver (and so the same mTLS client) as every other
/// sandbox call to that node.
pub struct ResolverNodeProbe {
    resolver: std::sync::Arc<dyn temps_agents::sandbox::node_routing::RemoteNodeResolver>,
    timeout: std::time::Duration,
}

impl ResolverNodeProbe {
    pub fn new(
        resolver: std::sync::Arc<dyn temps_agents::sandbox::node_routing::RemoteNodeResolver>,
    ) -> Self {
        Self {
            resolver,
            timeout: NODE_PROBE_TIMEOUT,
        }
    }
}

#[async_trait::async_trait]
impl NodeProbe for ResolverNodeProbe {
    async fn check(&self, node_id: i32) -> Result<(), String> {
        let ask = async {
            let provider = self
                .resolver
                .provider_for(node_id)
                .await
                .map_err(|e| e.to_string())?;
            if provider.is_available().await {
                return Ok(());
            }
            // `is_available` only says no; the status call says why (an
            // agent without sandbox support answers a bare 404, which the
            // remote provider reports as "upgrade temps on the node").
            match provider.image_status().await {
                Err(e) => Err(e.to_string()),
                Ok(_) => Err("Docker is not available on the node, so it cannot run \
                              sandboxes; check Docker on the node"
                    .to_string()),
            }
        };
        let answer = match tokio::time::timeout(self.timeout, ask).await {
            Ok(answer) => answer,
            Err(_) => Err(format!(
                "the node did not answer a sandbox status check within {}s",
                self.timeout.as_secs()
            )),
        };
        // Part of this text comes from the worker.
        answer.map_err(|reason| crate::services::sandbox_service::bounded_reason(&reason))
    }
}

/// Annotate every node (control plane first) with its placement state.
pub fn describe(
    policy: &PlacementPolicy,
    workers: &[WorkerNode],
    live: &HashMap<i32, u64>,
) -> Vec<PlacementNode> {
    let cp_allowed = policy.allows(CONTROL_PLANE_NODE_ID);
    let mut out = vec![PlacementNode {
        id: CONTROL_PLANE_NODE_ID,
        name: CONTROL_PLANE_NAME.to_string(),
        is_control_plane: true,
        status: PLACEABLE_STATUS.to_string(),
        allowed: cp_allowed,
        eligible: cp_allowed,
        reason: (!cp_allowed).then(|| "excluded by sandbox placement settings".to_string()),
        live_sandboxes: live.get(&CONTROL_PLANE_NODE_ID).copied().unwrap_or(0),
    }];
    for w in workers {
        let allowed = policy.allows(w.id);
        let placeable = w.placeable();
        let active = placeable.is_ok();
        let reason = match (allowed, placeable) {
            (false, _) => Some("excluded by sandbox placement settings".to_string()),
            (true, Err(why)) => Some(why.reason()),
            (true, Ok(())) => None,
        };
        out.push(PlacementNode {
            id: w.id,
            name: w.name.clone(),
            is_control_plane: false,
            status: w.status.clone(),
            allowed,
            eligible: allowed && active,
            reason,
            live_sandboxes: live.get(&w.id).copied().unwrap_or(0),
        });
    }
    out
}

/// Load every worker node, ordered by id.
pub async fn load_workers(db: &DatabaseConnection) -> Result<Vec<WorkerNode>, SandboxError> {
    let now = chrono::Utc::now();
    Ok(nodes::Entity::find()
        .order_by_asc(nodes::Column::Id)
        .all(db)
        .await?
        .into_iter()
        .filter_map(|n| classify(n, now))
        .collect())
}

/// Turn a node row into a sandbox worker, or `None` for the control plane,
/// which is never a sandbox worker even if it has a row. Build-only nodes,
/// nodes with a plain `http://` agent address and active nodes with stale
/// heartbeats are kept but blocked, so the operator sees why they don't take
/// sandboxes.
pub fn classify(n: nodes::Model, now: chrono::DateTime<chrono::Utc>) -> Option<WorkerNode> {
    if n.role == "control-plane" {
        return None;
    }
    let build_only = n
        .labels
        .get(NODE_ROLE_LABEL)
        .and_then(|v| v.as_str())
        .is_some_and(|role| role == BUILDER_NODE_ROLE);
    let stale = n
        .last_heartbeat
        .is_none_or(|at| (now - at).num_seconds() > HEARTBEAT_THRESHOLD_SECS);
    let blocked = if build_only {
        Some(Blocked::BuildOnly)
    } else if !is_https_address(&n.address) {
        Some(Blocked::PlainHttp)
    } else if n.status == PLACEABLE_STATUS && stale {
        Some(Blocked::NoHeartbeat)
    } else {
        None
    };
    Some(WorkerNode {
        id: n.id,
        name: n.name,
        status: n.status,
        blocked,
    })
}

#[derive(FromQueryResult)]
struct NodeCount {
    node_id: Option<i32>,
    count: i64,
}

/// Live (non-destroyed) sandbox count per node; the control plane is id 0.
pub async fn live_counts(db: &DatabaseConnection) -> Result<HashMap<i32, u64>, SandboxError> {
    let rows = sandboxes::Entity::find()
        .select_only()
        .column(sandboxes::Column::NodeId)
        .column_as(
            sea_orm::sea_query::Expr::col(sandboxes::Column::Id).count(),
            "count",
        )
        .filter(sandboxes::Column::Status.ne("destroyed"))
        .group_by(sandboxes::Column::NodeId)
        .into_model::<NodeCount>()
        .all(db)
        .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            (
                r.node_id.unwrap_or(CONTROL_PLANE_NODE_ID),
                r.count.max(0) as u64,
            )
        })
        .collect())
}

/// Display names for a set of node ids (missing ids are simply absent).
pub async fn node_names(
    db: &DatabaseConnection,
    ids: impl IntoIterator<Item = i32>,
) -> Result<HashMap<i32, String>, SandboxError> {
    let ids: Vec<i32> = ids.into_iter().collect();
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    Ok(nodes::Entity::find()
        .filter(nodes::Column::Id.is_in(ids))
        .all(db)
        .await?
        .into_iter()
        .map(|n| (n.id, n.name))
        .collect())
}

/// Validate an allow-list before it is saved: every id must be the control
/// plane or an existing node, with no duplicates.
pub fn validate_allowed_ids(ids: &[i32], workers: &[WorkerNode]) -> Result<(), SandboxError> {
    let mut seen = std::collections::HashSet::new();
    for id in ids {
        if !seen.insert(*id) {
            return Err(SandboxError::Validation {
                message: format!("node id {id} is listed more than once"),
            });
        }
        if *id != CONTROL_PLANE_NODE_ID && !workers.iter().any(|w| w.id == *id) {
            return Err(SandboxError::Validation {
                message: format!(
                    "node id {id} does not exist (use 0 for the control plane; list nodes with GET /sandboxes/placement)"
                ),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::Arc;

    pub(crate) fn node_row(
        role: &str,
        status: &str,
        heartbeat_age_secs: Option<i64>,
    ) -> nodes::Model {
        let now = chrono::Utc::now();
        nodes::Model {
            architecture: None,
            id: 7,
            name: "worker-7".to_string(),
            token_hash: "hash".to_string(),
            token_encrypted: None,
            address: "https://10.100.0.7:3100".to_string(),
            private_address: "10.100.0.7".to_string(),
            public_endpoint: None,
            wg_public_key: None,
            mesh_wg_public_key: None,
            mesh_wg_endpoint: None,
            mesh_wg_address: None,
            role: role.to_string(),
            status: status.to_string(),
            labels: serde_json::json!({}),
            capacity: serde_json::json!({}),
            last_heartbeat: heartbeat_age_secs.map(|s| now - chrono::Duration::seconds(s)),
            edge_public_key: None,
            compute_cidr: None,
            underlay_address: None,
            failover_at: None,
            dns_resolver_running: None,
            dns_resolver_tasks_alive: None,
            dns_resolver_last_sync_at: None,
            dns_resolver_consecutive_failures: 0,
            dns_resolver_last_error: None,
            dns_resolver_record_count: None,
            public_ingress_enabled: false,
            public_ingress_running: None,
            public_ingress_last_error: None,
            public_ingress_certificate_count: None,
            public_ingress_route_count: None,
            public_ingress_unsupported_route_count: None,
            public_ingress_unsupported_reasons: serde_json::json!([]),
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn classify_skips_the_control_plane_row() {
        let now = chrono::Utc::now();
        assert!(classify(node_row("control-plane", "active", Some(1)), now).is_none());
    }

    #[test]
    fn classify_places_a_healthy_worker() {
        let now = chrono::Utc::now();
        let w = classify(node_row("worker", "active", Some(10)), now).unwrap();
        assert_eq!((w.id, w.blocked), (7, None));
    }

    #[test]
    fn classify_blocks_build_only_nodes_whatever_their_health() {
        let now = chrono::Utc::now();
        let mut row = node_row("worker", "active", Some(10));
        row.labels = serde_json::json!({ "temps.sh/role": "builder" });
        let w = classify(row, now).unwrap();
        assert_eq!(w.blocked, Some(Blocked::BuildOnly));
    }

    /// Sandbox calls carry the node token, env vars and file contents, so a
    /// node whose agent is only reachable over plain http:// takes none.
    #[test]
    fn classify_blocks_plain_http_nodes() {
        let now = chrono::Utc::now();
        let mut row = node_row("worker", "active", Some(10));
        row.address = "http://10.100.0.7:3100".to_string();
        let w = classify(row, now).unwrap();
        assert_eq!(w.blocked, Some(Blocked::PlainHttp));
        let rows = describe(&all(), &[w], &HashMap::new());
        assert!(!rows[1].eligible);
        assert_eq!(
            rows[1].reason.as_deref(),
            Some("node address uses http://; sandboxes need an https (mTLS) node address")
        );

        let mut upper = node_row("worker", "active", Some(10));
        upper.address = " HTTPS://10.100.0.7:3100".to_string();
        assert_eq!(classify(upper, now).unwrap().blocked, None);
    }

    #[test]
    fn https_addresses_are_recognised() {
        assert!(is_https_address("https://10.0.0.1:3100"));
        assert!(is_https_address("HTTPS://node"));
        assert!(!is_https_address("http://10.0.0.1:3100"));
        assert!(!is_https_address("10.0.0.1:3100"));
        assert!(!is_https_address("https:/x"));
        assert!(!is_https_address(""));
    }

    #[test]
    fn classify_blocks_active_nodes_with_stale_or_missing_heartbeats() {
        let now = chrono::Utc::now();
        let stale = classify(
            node_row("worker", "active", Some(HEARTBEAT_THRESHOLD_SECS + 5)),
            now,
        )
        .unwrap();
        assert_eq!(stale.blocked, Some(Blocked::NoHeartbeat));
        let never = classify(node_row("worker", "active", None), now).unwrap();
        assert_eq!(never.blocked, Some(Blocked::NoHeartbeat));
        // A node already known to be offline reports its status instead.
        let offline = classify(node_row("worker", "offline", None), now).unwrap();
        assert_eq!(offline.blocked, None);
        assert_eq!(offline.placeable(), Err(Unplaceable::Status("offline")));
    }

    fn workers() -> Vec<WorkerNode> {
        vec![
            WorkerNode {
                id: 3,
                name: "worker-1".into(),
                status: "active".into(),
                blocked: None,
            },
            WorkerNode {
                id: 4,
                name: "worker-2".into(),
                status: "active".into(),
                blocked: None,
            },
            WorkerNode {
                id: 5,
                name: "worker-3".into(),
                status: "offline".into(),
                blocked: None,
            },
        ]
    }

    fn all() -> PlacementPolicy {
        PlacementPolicy::default()
    }

    fn only(ids: &[i32]) -> PlacementPolicy {
        PlacementPolicy {
            allowed_node_ids: Some(ids.to_vec()),
        }
    }

    #[test]
    fn parses_node_references() {
        assert_eq!(
            RequestedNode::parse("control-plane"),
            RequestedNode::ControlPlane
        );
        assert_eq!(RequestedNode::parse("0"), RequestedNode::ControlPlane);
        assert_eq!(RequestedNode::parse(" 7 "), RequestedNode::Id(7));
        assert_eq!(
            RequestedNode::parse("worker-1"),
            RequestedNode::Name("worker-1".into())
        );
        assert_eq!(RequestedNode::parse("-3"), RequestedNode::Name("-3".into()));
    }

    #[test]
    fn default_placement_keeps_single_node_behaviour() {
        let live = HashMap::new();
        assert_eq!(choose(&all(), &[], &live, None).unwrap(), None);
        assert_eq!(choose(&all(), &workers(), &live, None).unwrap(), None);
    }

    #[test]
    fn default_placement_without_control_plane_picks_least_loaded_active_worker() {
        let live = HashMap::from([(3, 2), (4, 1), (5, 0)]);
        // worker-3 has 0 but is offline; worker-2 has fewer than worker-1.
        assert_eq!(
            choose(&only(&[3, 4, 5]), &workers(), &live, None).unwrap(),
            Some(4)
        );
        // Tie → lowest id.
        let live = HashMap::from([(3, 1), (4, 1)]);
        assert_eq!(
            choose(&only(&[3, 4]), &workers(), &live, None).unwrap(),
            Some(3)
        );
    }

    #[test]
    fn no_eligible_node_is_an_error() {
        let err = choose(&only(&[5]), &workers(), &HashMap::new(), None).unwrap_err();
        assert!(matches!(err, SandboxError::NoPlacementNode));
        let err = choose(&only(&[]), &workers(), &HashMap::new(), None).unwrap_err();
        assert!(matches!(err, SandboxError::NoPlacementNode));
    }

    #[test]
    fn explicit_node_by_name_or_id() {
        let live = HashMap::new();
        let by_name = RequestedNode::parse("worker-2");
        assert_eq!(
            choose(&all(), &workers(), &live, Some(&by_name)).unwrap(),
            Some(4)
        );
        let by_id = RequestedNode::parse("3");
        assert_eq!(
            choose(&all(), &workers(), &live, Some(&by_id)).unwrap(),
            Some(3)
        );
        let cp = RequestedNode::parse("control-plane");
        assert_eq!(choose(&all(), &workers(), &live, Some(&cp)).unwrap(), None);
    }

    #[test]
    fn explicit_node_never_falls_back() {
        let live = HashMap::new();
        let missing = RequestedNode::parse("nope");
        assert!(matches!(
            choose(&all(), &workers(), &live, Some(&missing)).unwrap_err(),
            SandboxError::NodeNotFound { .. }
        ));
        let offline = RequestedNode::parse("worker-3");
        assert!(matches!(
            choose(&all(), &workers(), &live, Some(&offline)).unwrap_err(),
            SandboxError::NodeNotReady { .. }
        ));
        let disallowed = RequestedNode::parse("worker-2");
        assert!(matches!(
            choose(&only(&[0, 3]), &workers(), &live, Some(&disallowed)).unwrap_err(),
            SandboxError::NodeNotAllowed { .. }
        ));
        let cp = RequestedNode::parse("control-plane");
        assert!(matches!(
            choose(&only(&[3]), &workers(), &live, Some(&cp)).unwrap_err(),
            SandboxError::NodeNotAllowed { .. }
        ));
    }

    #[test]
    fn blocked_nodes_are_skipped_and_refused_by_name() {
        let mut ws = workers();
        // worker-1 (id 3) is build-only; worker-2 (id 4) is the only choice.
        ws[0].blocked = Some(Blocked::BuildOnly);
        let live = HashMap::new();
        assert_eq!(choose(&only(&[3, 4]), &ws, &live, None).unwrap(), Some(4));
        let err = choose(
            &all(),
            &ws,
            &live,
            Some(&RequestedNode::Name("worker-1".into())),
        )
        .unwrap_err();
        assert!(
            matches!(&err, SandboxError::NodeNotReady { status, .. } if status == "a build-only node"),
            "{err:?}"
        );
        let rows = describe(&all(), &ws, &live);
        let w1 = rows.iter().find(|n| n.id == 3).unwrap();
        assert!(!w1.eligible);
        assert_eq!(w1.reason.as_deref(), Some("node is a build-only node"));
    }

    #[test]
    fn existing_node_ids_drops_removed_nodes_only() {
        assert_eq!(existing_node_ids(None, &workers()), None);
        assert_eq!(
            existing_node_ids(Some(vec![0, 3, 42, 5]), &workers()),
            Some(vec![0, 3, 5])
        );
        assert_eq!(existing_node_ids(Some(vec![]), &workers()), Some(vec![]));
    }

    #[test]
    fn find_node_resolves_control_plane_ids_and_names() {
        let rows = || describe(&all(), &workers(), &HashMap::new());
        let find = |raw: &str| find_node(rows(), &RequestedNode::parse(raw)).map(|n| n.id);
        assert_eq!(find("control-plane"), Some(CONTROL_PLANE_NODE_ID));
        assert_eq!(find("0"), Some(CONTROL_PLANE_NODE_ID));
        assert_eq!(find("4"), Some(4));
        assert_eq!(find("worker-3"), Some(5), "offline nodes are still listed");
        assert_eq!(find("worker-9"), None);
        assert_eq!(find("9"), None);
        // The control plane's display name is not a worker name.
        let by_name = RequestedNode::Name(CONTROL_PLANE_NAME.into());
        assert!(find_node(rows(), &by_name).is_none());
    }

    #[test]
    fn describe_marks_eligibility_and_reasons() {
        let rows = describe(&only(&[0, 3, 5]), &workers(), &HashMap::from([(3, 2)]));
        assert_eq!(rows.len(), 4);
        assert!(rows[0].is_control_plane && rows[0].eligible);
        assert!(rows[1].eligible && rows[1].live_sandboxes == 2);
        assert!(!rows[2].allowed && !rows[2].eligible);
        assert!(rows[3].allowed && !rows[3].eligible);
        assert_eq!(rows[3].reason.as_deref(), Some("node is offline"));
    }

    #[test]
    fn allow_list_validation() {
        assert!(validate_allowed_ids(&[0, 3], &workers()).is_ok());
        assert!(validate_allowed_ids(&[], &workers()).is_ok());
        assert!(validate_allowed_ids(&[99], &workers()).is_err());
        assert!(validate_allowed_ids(&[3, 3], &workers()).is_err());
    }

    /// A node whose sandboxes are being evicted takes no new ones: automatic
    /// placement skips it, an explicit request is refused naming it, and the
    /// listing says why.
    #[test]
    fn a_node_being_evicted_is_not_placeable() {
        let mut ws = workers();
        mark_evicting(&mut ws, &std::collections::HashSet::from([3]));
        let live = HashMap::new();

        assert_eq!(choose(&only(&[3, 4]), &ws, &live, None).unwrap(), Some(4));
        let err = choose(&all(), &ws, &live, Some(&RequestedNode::Id(3))).unwrap_err();
        assert!(
            matches!(&err, SandboxError::NodeNotReady { node, .. } if node == "worker-1"),
            "{err:?}"
        );
        assert!(
            err.to_string().contains("having its sandboxes evicted"),
            "{err}"
        );
        let rows = describe(&all(), &ws, &live);
        let w1 = rows.iter().find(|n| n.id == 3).unwrap();
        assert!(!w1.eligible);
        assert_eq!(
            w1.reason.as_deref(),
            Some("sandboxes on this node are being evicted")
        );
        // Only the evicted node is affected.
        assert!(rows.iter().find(|n| n.id == 4).unwrap().eligible);
    }

    #[test]
    fn candidates_rank_every_eligible_worker() {
        let live = HashMap::from([(3, 2), (4, 1)]);
        let mut ws = workers();
        ws.push(WorkerNode {
            id: 6,
            name: "worker-6".into(),
            status: "active".into(),
            blocked: None,
        });
        assert_eq!(
            candidates(&only(&[3, 4, 5, 6]), &ws, &live, None).unwrap(),
            Candidates::Ranked(vec![
                (6, "worker-6".into()),
                (4, "worker-2".into()),
                (3, "worker-1".into()),
            ])
        );
        assert_eq!(
            candidates(&all(), &ws, &live, None).unwrap(),
            Candidates::ControlPlane
        );
        assert_eq!(
            candidates(
                &all(),
                &ws,
                &live,
                Some(&RequestedNode::Name("worker-2".into()))
            )
            .unwrap(),
            Candidates::Requested {
                id: 4,
                name: "worker-2".into()
            }
        );
    }

    /// Answers per node id and records which nodes were asked.
    struct FakeProbe {
        answers: HashMap<i32, Result<(), String>>,
        asked: std::sync::Mutex<Vec<i32>>,
    }

    impl FakeProbe {
        fn new(answers: &[(i32, Result<(), &str>)]) -> Self {
            Self {
                answers: answers
                    .iter()
                    .map(|(id, r)| (*id, r.map_err(str::to_string)))
                    .collect(),
                asked: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn asked(&self) -> Vec<i32> {
            self.asked.lock().expect("asked").clone()
        }
    }

    #[async_trait::async_trait]
    impl NodeProbe for FakeProbe {
        async fn check(&self, node_id: i32) -> Result<(), String> {
            self.asked.lock().expect("asked").push(node_id);
            self.answers
                .get(&node_id)
                .cloned()
                .unwrap_or_else(|| Err("not configured".to_string()))
        }
    }

    #[tokio::test]
    async fn the_control_plane_is_never_probed() {
        let probe = FakeProbe::new(&[]);
        assert_eq!(
            settle(Candidates::ControlPlane, &probe).await.unwrap(),
            None
        );
        assert!(probe.asked().is_empty());
    }

    #[tokio::test]
    async fn default_placement_skips_a_worker_that_cannot_run_sandboxes() {
        let probe = FakeProbe::new(&[(6, Err("Docker is not available on the node")), (4, Ok(()))]);
        let ranked = Candidates::Ranked(vec![
            (6, "worker-6".into()),
            (4, "worker-2".into()),
            (3, "worker-1".into()),
        ]);

        assert_eq!(settle(ranked, &probe).await.unwrap(), Some(4));
        assert_eq!(probe.asked(), vec![6, 4], "stops at the first that works");
    }

    #[tokio::test]
    async fn default_placement_gives_up_after_a_bounded_number_of_workers() {
        let probe = FakeProbe::new(&[
            (1, Err("Docker is not available on the node")),
            (2, Err("upgrade temps on the node")),
            (3, Err("did not answer")),
            (4, Ok(())),
        ]);
        let ranked = Candidates::Ranked((1..=4).map(|id| (id, format!("worker-{id}"))).collect());

        let err = settle(ranked, &probe).await.unwrap_err();

        assert_eq!(probe.asked(), vec![1, 2, 3]);
        assert_eq!(MAX_PROBED_CANDIDATES, 3);
        let message = err.to_string();
        assert!(
            matches!(err, SandboxError::NoReadyPlacementNode { .. }),
            "{message}"
        );
        for expected in [
            "node 'worker-1': Docker is not available on the node",
            "node 'worker-2': upgrade temps on the node",
            "node 'worker-3': did not answer",
        ] {
            assert!(message.contains(expected), "{message}");
        }
    }

    #[tokio::test]
    async fn a_requested_worker_that_cannot_run_sandboxes_is_refused_by_name() {
        let probe = FakeProbe::new(&[(4, Err("upgrade temps on the node"))]);
        let requested = Candidates::Requested {
            id: 4,
            name: "worker-2".into(),
        };

        let err = settle(requested, &probe).await.unwrap_err();

        assert!(
            matches!(&err, SandboxError::NodeNotReady { node, status }
                if node == "worker-2" && status.contains("upgrade temps on the node")),
            "{err:?}"
        );
        assert_eq!(probe.asked(), vec![4], "no fallback to another node");
    }

    #[tokio::test]
    async fn a_requested_worker_that_can_run_sandboxes_is_used() {
        let probe = FakeProbe::new(&[(4, Ok(()))]);
        let requested = Candidates::Requested {
            id: 4,
            name: "worker-2".into(),
        };
        assert_eq!(settle(requested, &probe).await.unwrap(), Some(4));
    }

    /// A resolver whose node never answers, or answers as configured.
    struct StaticResolver(Arc<dyn temps_agents::sandbox::SandboxProvider>);

    #[async_trait::async_trait]
    impl temps_agents::sandbox::node_routing::RemoteNodeResolver for StaticResolver {
        async fn provider_for(
            &self,
            _node_id: i32,
        ) -> Result<Arc<dyn temps_agents::sandbox::SandboxProvider>, temps_agents::error::AgentError>
        {
            Ok(self.0.clone())
        }
    }

    struct UnresolvableNode;

    #[async_trait::async_trait]
    impl temps_agents::sandbox::node_routing::RemoteNodeResolver for UnresolvableNode {
        async fn provider_for(
            &self,
            node_id: i32,
        ) -> Result<Arc<dyn temps_agents::sandbox::SandboxProvider>, temps_agents::error::AgentError>
        {
            Err(temps_agents::error::AgentError::SandboxNodeUnavailable {
                node_id,
                node_name: "worker-2".into(),
                reason: "the node is offline".into(),
            })
        }
    }

    #[tokio::test]
    async fn resolver_probe_reports_why_a_node_cannot_run_sandboxes() {
        // A node the resolver refuses (offline, http://, no token): its
        // reason is passed on.
        let probe = ResolverNodeProbe::new(Arc::new(UnresolvableNode));
        let reason = probe.check(4).await.unwrap_err();
        assert!(reason.contains("the node is offline"), "{reason}");

        // A provider that reports itself available passes.
        let probe = ResolverNodeProbe::new(Arc::new(StaticResolver(Arc::new(
            temps_agents::sandbox::local::LocalSandboxProvider::new(),
        ))));
        assert_eq!(probe.check(4).await, Ok(()));
    }

    #[tokio::test]
    async fn resolver_probe_gives_up_on_a_node_that_does_not_answer() {
        struct Hangs;
        #[async_trait::async_trait]
        impl temps_agents::sandbox::node_routing::RemoteNodeResolver for Hangs {
            async fn provider_for(
                &self,
                _node_id: i32,
            ) -> Result<
                Arc<dyn temps_agents::sandbox::SandboxProvider>,
                temps_agents::error::AgentError,
            > {
                std::future::pending().await
            }
        }
        let probe = ResolverNodeProbe {
            resolver: Arc::new(Hangs),
            timeout: std::time::Duration::from_millis(20),
        };
        let reason = probe.check(4).await.unwrap_err();
        assert!(reason.contains("did not answer"), "{reason}");
    }
}
