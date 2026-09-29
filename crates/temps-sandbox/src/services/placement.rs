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

/// A worker as far as placement cares.
#[derive(Debug, Clone)]
pub struct WorkerNode {
    pub id: i32,
    pub name: String,
    pub status: String,
    /// Why an otherwise active node cannot take sandboxes (a build-only
    /// node, no recent heartbeat), phrased to follow "node X is …".
    pub blocked: Option<String>,
}

impl WorkerNode {
    /// `Err(reason)` when the node cannot take a new sandbox right now.
    fn placeable(&self) -> Result<(), String> {
        if self.status != PLACEABLE_STATUS {
            return Err(self.status.clone());
        }
        match &self.blocked {
            Some(reason) => Err(reason.clone()),
            None => Ok(()),
        }
    }
}

/// Node label marking a build-only node; same contract as the deploy
/// scheduler (`temps_deployments::services::node_scheduler`).
const NODE_ROLE_LABEL: &str = "temps.sh/role";
const BUILDER_NODE_ROLE: &str = "builder";

/// A node that has not sent a heartbeat for this long is treated as down,
/// matching the deploy scheduler's threshold.
const HEARTBEAT_THRESHOLD_SECS: i64 = 90;

/// Pure placement decision, separated from the database for testing.
/// `live` maps node id → live sandbox count (control plane under id 0).
pub fn choose(
    policy: &PlacementPolicy,
    workers: &[WorkerNode],
    live: &HashMap<i32, u64>,
    requested: Option<&RequestedNode>,
) -> Result<Option<i32>, SandboxError> {
    match requested {
        Some(RequestedNode::ControlPlane) => {
            if policy.allows(CONTROL_PLANE_NODE_ID) {
                Ok(None)
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
            if let Err(status) = node.placeable() {
                return Err(SandboxError::NodeNotReady {
                    node: node.name.clone(),
                    status,
                });
            }
            Ok(Some(node.id))
        }
        None => {
            if policy.allows(CONTROL_PLANE_NODE_ID) {
                return Ok(None);
            }
            workers
                .iter()
                .filter(|w| policy.allows(w.id) && w.placeable().is_ok())
                .min_by_key(|w| (live.get(&w.id).copied().unwrap_or(0), w.id))
                .map(|w| Some(w.id))
                .ok_or(SandboxError::NoPlacementNode)
        }
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
            (true, Err(why)) => Some(format!("node is {}", why)),
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
/// which is never a sandbox worker even if it has a row. Build-only nodes
/// and active nodes with stale heartbeats are kept but blocked, so the
/// operator sees why they don't take sandboxes.
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
        Some("a build-only node".to_string())
    } else if n.status == PLACEABLE_STATUS && stale {
        Some("not sending heartbeats".to_string())
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
mod tests {
    use super::*;

    fn node_row(role: &str, status: &str, heartbeat_age_secs: Option<i64>) -> nodes::Model {
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
        assert_eq!(w.blocked.as_deref(), Some("a build-only node"));
    }

    #[test]
    fn classify_blocks_active_nodes_with_stale_or_missing_heartbeats() {
        let now = chrono::Utc::now();
        let stale = classify(
            node_row("worker", "active", Some(HEARTBEAT_THRESHOLD_SECS + 5)),
            now,
        )
        .unwrap();
        assert_eq!(stale.blocked.as_deref(), Some("not sending heartbeats"));
        let never = classify(node_row("worker", "active", None), now).unwrap();
        assert_eq!(never.blocked.as_deref(), Some("not sending heartbeats"));
        // A node already known to be offline reports its status instead.
        let offline = classify(node_row("worker", "offline", None), now).unwrap();
        assert_eq!(offline.blocked, None);
        assert_eq!(offline.placeable(), Err("offline".to_string()));
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
        ws[0].blocked = Some("a build-only node".into());
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
}
