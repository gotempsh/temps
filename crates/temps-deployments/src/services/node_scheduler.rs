//! Node scheduler — selects which nodes each replica deploys to.
//!
//! **Single-node invariant:** if `nodes` table has zero active rows,
//! all replicas deploy locally (identical to current behavior).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use super::node_service::{NodeError, NodeService};

/// Describes where a replica should be deployed.
#[derive(Debug, Clone)]
pub enum NodeAssignment {
    /// Deploy to the local Docker daemon (single-node mode or control plane).
    Local,
    /// Deploy to a remote worker node.
    Remote {
        node_id: i32,
        node_name: String,
        /// Agent API URL
        address: String,
        /// WireGuard or private IP
        private_address: String,
    },
}

impl NodeAssignment {
    /// Returns true if this is a local deployment.
    pub fn is_local(&self) -> bool {
        matches!(self, NodeAssignment::Local)
    }

    /// Get the node_id if this is a remote assignment, None if local.
    pub fn node_id(&self) -> Option<i32> {
        match self {
            NodeAssignment::Local => None,
            NodeAssignment::Remote { node_id, .. } => Some(*node_id),
        }
    }

    /// Get the private address for cross-node networking.
    /// Returns None for local assignments.
    pub fn private_address(&self) -> Option<&str> {
        match self {
            NodeAssignment::Local => None,
            NodeAssignment::Remote {
                private_address, ..
            } => Some(private_address),
        }
    }
}

/// Scheduling strategy for distributing replicas across nodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SchedulingStrategy {
    /// Pure round-robin — ignores resource utilization.
    RoundRobin,
    /// Prefer nodes with the lowest resource utilization.
    /// Falls back to round-robin when no capacity data is available.
    #[default]
    LeastLoaded,
}

/// Default maximum load score (0.0–1.0). Nodes at or above this threshold
/// are excluded from scheduling unless all nodes exceed it.
const DEFAULT_MAX_LOAD_THRESHOLD: f64 = 0.90;

/// Schedules replicas across available nodes using resource-aware placement.
///
/// Default strategy is `LeastLoaded`: each replica is assigned to the node
/// with the lowest combined resource utilization (CPU + memory). When capacity
/// data is unavailable for all nodes, it falls back to round-robin.
///
/// Nodes whose load score exceeds `max_load_threshold` (default 90%) are
/// excluded from the scheduling pool. If **all** nodes exceed the threshold,
/// the limit is relaxed so that scheduling can still proceed (best-effort).
pub struct NodeScheduler {
    node_service: Arc<NodeService>,
    /// Round-robin counter (fallback when no capacity data available)
    counter: AtomicUsize,
    /// Heartbeat threshold in seconds — nodes with older heartbeats are excluded
    heartbeat_threshold_secs: i64,
    /// Scheduling strategy
    strategy: SchedulingStrategy,
    /// Nodes with a load score at or above this value are excluded from the
    /// scheduling pool. Range 0.0–1.0. Defaults to 0.90 (90%).
    max_load_threshold: f64,
}

impl NodeScheduler {
    pub fn new(node_service: Arc<NodeService>) -> Self {
        Self {
            node_service,
            counter: AtomicUsize::new(0),
            heartbeat_threshold_secs: 90,
            strategy: SchedulingStrategy::default(),
            max_load_threshold: DEFAULT_MAX_LOAD_THRESHOLD,
        }
    }

    pub fn with_strategy(mut self, strategy: SchedulingStrategy) -> Self {
        self.strategy = strategy;
        self
    }

    /// Set the maximum load threshold. Nodes at or above this score are
    /// excluded from scheduling. Must be between 0.0 and 1.0.
    pub fn with_max_load_threshold(mut self, threshold: f64) -> Self {
        self.max_load_threshold = threshold.clamp(0.0, 1.0);
        self
    }

    /// Get a reference to the underlying node service.
    pub fn node_service(&self) -> &NodeService {
        &self.node_service
    }

    /// Schedule `replica_count` replicas across available nodes.
    ///
    /// Returns a Vec of `NodeAssignment` — one per replica.
    ///
    /// **Behavior:**
    /// - If `target_node_ids` is set → only schedule on those nodes (error if none are active)
    /// - If `labels` is set → filter nodes by label matching:
    ///   - **Same key with array value** → OR (node must match any value)
    ///   - **Different keys** → AND (node must satisfy all keys)
    ///   - Example: `{"region": ["us", "asia"], "gpu": "true"}` matches nodes
    ///     where (region=us OR region=asia) AND gpu=true
    /// - If no active worker nodes exist → all replicas are `Local`
    /// - If active workers exist → distribute across **local + remote** nodes,
    ///   using the configured scheduling strategy (LeastLoaded by default)
    pub async fn schedule_replicas(
        &self,
        replica_count: u32,
        labels: Option<&serde_json::Value>,
        target_node_ids: Option<&[i32]>,
        anti_affinity: bool,
    ) -> Result<Vec<NodeAssignment>, NodeError> {
        self.schedule_replicas_excluding(replica_count, labels, target_node_ids, anti_affinity, &[])
            .await
    }

    /// Like [`schedule_replicas`] but additionally accepts `exclude_node_ids` —
    /// nodes that already host containers for the current environment. During a
    /// rolling update, the outgoing containers haven't been removed yet, so the
    /// scheduler should avoid those nodes when anti-affinity is enabled.
    ///
    /// Excluded nodes are removed from the pool **only** when anti-affinity is
    /// on and there are enough remaining nodes. If excluding them would leave
    /// zero nodes, the exclusion is relaxed (best-effort).
    pub async fn schedule_replicas_excluding(
        &self,
        replica_count: u32,
        labels: Option<&serde_json::Value>,
        target_node_ids: Option<&[i32]>,
        anti_affinity: bool,
        exclude_node_ids: &[i32],
    ) -> Result<Vec<NodeAssignment>, NodeError> {
        let active_nodes = self
            .node_service
            .list_active(self.heartbeat_threshold_secs)
            .await?;

        // Filter by target node IDs if specified
        let mut eligible_nodes: Vec<_> = if let Some(target_ids) = target_node_ids {
            active_nodes
                .into_iter()
                .filter(|n| target_ids.contains(&n.id))
                .collect()
        } else {
            active_nodes
        };

        // Filter by label selectors if specified
        if let Some(selector) = labels {
            if let Some(selector_map) = selector.as_object() {
                if !selector_map.is_empty() {
                    eligible_nodes.retain(|node| node_matches_labels(&node.labels, selector));
                }
            }
        }

        if eligible_nodes.is_empty() {
            if target_node_ids.is_some() {
                tracing::warn!(
                    "No active target nodes found for deployment, falling back to local deployment"
                );
            }
            return Ok(vec![NodeAssignment::Local; replica_count as usize]);
        }

        // Build the scheduling pool: Local (control plane) + remote worker nodes
        let full_pool: Vec<PoolEntry> = std::iter::once(PoolEntry {
            assignment: NodeAssignment::Local,
            load_score: None, // Local node has no capacity reporting
        })
        .chain(eligible_nodes.iter().map(|node| PoolEntry {
            assignment: NodeAssignment::Remote {
                node_id: node.id,
                node_name: node.name.clone(),
                address: node.address.clone(),
                private_address: node.private_address.clone(),
            },
            load_score: compute_load_score(&node.capacity),
        }))
        .collect();

        // Filter out nodes that exceed the max load threshold.
        // Nodes without capacity data (load_score = None) are always eligible.
        // If ALL nodes with capacity data exceed the threshold, relax the limit
        // so scheduling can still proceed (best-effort).
        let pool: Vec<PoolEntry> = {
            let under_threshold: Vec<PoolEntry> = full_pool
                .into_iter()
                .filter(|e| match e.load_score {
                    Some(score) => score < self.max_load_threshold,
                    None => true, // No data → eligible
                })
                .collect();

            if under_threshold.is_empty() {
                tracing::warn!(
                    threshold = self.max_load_threshold,
                    "All nodes exceed max load threshold ({:.0}%), scheduling best-effort",
                    self.max_load_threshold * 100.0
                );
                // Re-build full pool (moved above)
                std::iter::once(PoolEntry {
                    assignment: NodeAssignment::Local,
                    load_score: None,
                })
                .chain(eligible_nodes.iter().map(|node| PoolEntry {
                    assignment: NodeAssignment::Remote {
                        node_id: node.id,
                        node_name: node.name.clone(),
                        address: node.address.clone(),
                        private_address: node.private_address.clone(),
                    },
                    load_score: compute_load_score(&node.capacity),
                }))
                .collect()
            } else {
                under_threshold
            }
        };

        // When anti-affinity is enabled, exclude nodes that already host containers
        // for the current environment (rolling update awareness). If excluding them
        // would leave zero nodes, relax the exclusion.
        let pool = if anti_affinity && !exclude_node_ids.is_empty() {
            let filtered: Vec<PoolEntry> = pool
                .into_iter()
                .filter(|e| {
                    // Local node is excluded if exclude_node_ids contains a sentinel
                    // value (not applicable — Local has no node_id), so always keep it.
                    match &e.assignment {
                        NodeAssignment::Local => true,
                        NodeAssignment::Remote { node_id, .. } => {
                            !exclude_node_ids.contains(node_id)
                        }
                    }
                })
                .collect();

            if filtered.is_empty() {
                tracing::warn!(
                    excluded = ?exclude_node_ids,
                    "All nodes are excluded for anti-affinity, relaxing exclusion"
                );
                // Re-build: we can't use pool since it was moved, rebuild from eligible_nodes
                std::iter::once(PoolEntry {
                    assignment: NodeAssignment::Local,
                    load_score: None,
                })
                .chain(eligible_nodes.iter().map(|node| PoolEntry {
                    assignment: NodeAssignment::Remote {
                        node_id: node.id,
                        node_name: node.name.clone(),
                        address: node.address.clone(),
                        private_address: node.private_address.clone(),
                    },
                    load_score: compute_load_score(&node.capacity),
                }))
                .collect()
            } else {
                filtered
            }
        } else {
            pool
        };

        let assignments = if anti_affinity && pool.len() > 1 {
            // Anti-affinity: spread replicas across different nodes first,
            // then wrap around when we have more replicas than nodes.
            match self.strategy {
                SchedulingStrategy::RoundRobin => {
                    schedule_anti_affinity_round_robin(&pool, replica_count)
                }
                SchedulingStrategy::LeastLoaded => {
                    let has_capacity = pool.iter().any(|e| e.load_score.is_some());
                    if has_capacity {
                        schedule_anti_affinity_least_loaded(&pool, replica_count)
                    } else {
                        schedule_anti_affinity_round_robin(&pool, replica_count)
                    }
                }
            }
        } else {
            match self.strategy {
                SchedulingStrategy::RoundRobin => self.schedule_round_robin(&pool, replica_count),
                SchedulingStrategy::LeastLoaded => {
                    let has_capacity = pool.iter().any(|e| e.load_score.is_some());
                    if has_capacity {
                        schedule_least_loaded(&pool, replica_count)
                    } else {
                        self.schedule_round_robin(&pool, replica_count)
                    }
                }
            }
        };

        Ok(assignments)
    }

    /// Get the assignment for a single replica (convenience wrapper).
    pub async fn schedule_single(
        &self,
        labels: Option<&serde_json::Value>,
    ) -> Result<NodeAssignment, NodeError> {
        let mut assignments = self.schedule_replicas(1, labels, None, false).await?;
        Ok(assignments.remove(0))
    }

    fn schedule_round_robin(&self, pool: &[PoolEntry], replica_count: u32) -> Vec<NodeAssignment> {
        let mut assignments = Vec::with_capacity(replica_count as usize);
        for _ in 0..replica_count {
            let idx = self.counter.fetch_add(1, Ordering::Relaxed) % pool.len();
            assignments.push(pool[idx].assignment.clone());
        }
        assignments
    }
}

/// Entry in the scheduling pool with optional resource load score.
struct PoolEntry {
    assignment: NodeAssignment,
    /// Load score 0.0–1.0 where 0.0 = idle, 1.0 = fully loaded.
    /// None means no capacity data is available.
    load_score: Option<f64>,
}

/// Compute a load score from a node's capacity JSON.
///
/// Returns a score between 0.0 (idle) and 1.0 (fully loaded).
/// The score is a weighted average: 60% CPU, 40% memory.
/// Returns None if no usable metrics are present.
fn compute_load_score(capacity: &serde_json::Value) -> Option<f64> {
    let obj = capacity.as_object()?;

    let cpu_percent = obj.get("cpu_percent").and_then(|v| v.as_f64());

    let mem_percent = match (
        obj.get("memory_used_bytes").and_then(|v| v.as_f64()),
        obj.get("memory_total_bytes").and_then(|v| v.as_f64()),
    ) {
        (Some(used), Some(total)) if total > 0.0 => Some((used / total) * 100.0),
        _ => None,
    };

    match (cpu_percent, mem_percent) {
        (Some(cpu), Some(mem)) => Some((cpu * 0.6 + mem * 0.4) / 100.0),
        (Some(cpu), None) => Some(cpu / 100.0),
        (None, Some(mem)) => Some(mem / 100.0),
        (None, None) => None,
    }
}

/// Assign replicas to the least-loaded nodes.
///
/// For each replica, picks the node with the lowest effective load score.
/// Nodes without capacity data get a neutral score of 0.5.
/// After assigning a replica, the node's effective score is bumped by a
/// small increment to spread replicas across nodes with similar load.
fn schedule_least_loaded(pool: &[PoolEntry], replica_count: u32) -> Vec<NodeAssignment> {
    // Effective scores — mutated as we assign replicas
    let mut scores: Vec<f64> = pool.iter().map(|e| e.load_score.unwrap_or(0.5)).collect();

    // Bump per assignment: roughly "one replica's worth" of load spread
    // across the pool. This prevents piling all replicas on one low-load node.
    let bump = if pool.is_empty() {
        0.0
    } else {
        1.0 / pool.len() as f64
    };

    let mut assignments = Vec::with_capacity(replica_count as usize);

    for _ in 0..replica_count {
        // Find the index with the lowest score
        let best_idx = scores
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(i, _)| i)
            .unwrap_or(0);

        assignments.push(pool[best_idx].assignment.clone());
        scores[best_idx] += bump;
    }

    assignments
}

/// Anti-affinity round-robin: assign each replica to a different node before
/// wrapping around. Guarantees no two replicas share a node until all nodes
/// have been used once.
fn schedule_anti_affinity_round_robin(
    pool: &[PoolEntry],
    replica_count: u32,
) -> Vec<NodeAssignment> {
    let mut assignments = Vec::with_capacity(replica_count as usize);
    for i in 0..replica_count {
        let idx = (i as usize) % pool.len();
        assignments.push(pool[idx].assignment.clone());
    }
    assignments
}

/// Anti-affinity least-loaded: assign each replica to the least-loaded node
/// that hasn't been used yet in the current round. Once all nodes are used,
/// start a new round with reset tracking.
fn schedule_anti_affinity_least_loaded(
    pool: &[PoolEntry],
    replica_count: u32,
) -> Vec<NodeAssignment> {
    let mut scores: Vec<f64> = pool.iter().map(|e| e.load_score.unwrap_or(0.5)).collect();

    let mut assignments = Vec::with_capacity(replica_count as usize);
    let mut used_in_round: Vec<bool> = vec![false; pool.len()];
    let mut used_count = 0;

    for _ in 0..replica_count {
        // If all nodes used in this round, reset for next round
        if used_count >= pool.len() {
            used_in_round.fill(false);
            used_count = 0;
        }

        // Find the least-loaded node that hasn't been used in this round
        let best_idx = scores
            .iter()
            .enumerate()
            .filter(|(i, _)| !used_in_round[*i])
            .min_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(i, _)| i)
            .unwrap_or(0);

        assignments.push(pool[best_idx].assignment.clone());
        used_in_round[best_idx] = true;
        used_count += 1;

        // Bump score for future rounds
        let bump = if pool.is_empty() {
            0.0
        } else {
            1.0 / pool.len() as f64
        };
        scores[best_idx] += bump;
    }

    assignments
}

/// Check if a node's labels match a label selector.
///
/// Matching rules:
/// - **Same key, array value** → OR: node label must match any value in the array
/// - **Same key, string value** → exact match
/// - **Different keys** → AND: node must satisfy all keys in the selector
///
/// Example selector: `{"region": ["us", "asia"], "gpu": "true"}`
/// Matches a node with `{"region": "us", "gpu": "true"}` (region=us OR region=asia, AND gpu=true)
fn node_matches_labels(node_labels: &serde_json::Value, selector: &serde_json::Value) -> bool {
    let selector_map = match selector.as_object() {
        Some(m) => m,
        None => return true, // Non-object selector matches everything
    };

    let node_map = match node_labels.as_object() {
        Some(m) => m,
        None => return selector_map.is_empty(), // No node labels → only matches empty selector
    };

    // Every key in the selector must be satisfied (AND across keys)
    for (key, required_value) in selector_map {
        let node_value = match node_map.get(key) {
            Some(v) => v,
            None => return false, // Node doesn't have this label key → no match
        };

        let key_matches = if let Some(required_values) = required_value.as_array() {
            // Array → OR: node value must match any element
            required_values
                .iter()
                .any(|rv| values_match(node_value, rv))
        } else {
            // Single value → exact match
            values_match(node_value, required_value)
        };

        if !key_matches {
            return false;
        }
    }

    true
}

/// Compare two JSON values for label matching.
/// Compares as strings when possible for flexibility (e.g., `"true"` matches `"true"`).
fn values_match(node_value: &serde_json::Value, required_value: &serde_json::Value) -> bool {
    match (node_value, required_value) {
        (serde_json::Value::String(a), serde_json::Value::String(b)) => a == b,
        // Allow comparing string node labels against non-string selectors via to_string
        _ => {
            node_value.to_string().trim_matches('"') == required_value.to_string().trim_matches('"')
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use temps_entities::nodes;

    fn make_node(id: i32, name: &str) -> nodes::Model {
        nodes::Model {
            id,
            name: name.to_string(),
            token_hash: "hash".to_string(),
            token_encrypted: None,
            address: format!("https://10.100.0.{}:3100", id),
            private_address: format!("10.100.0.{}", id),
            public_endpoint: None,
            wg_public_key: None,
            role: "worker".to_string(),
            status: "active".to_string(),
            labels: serde_json::json!({}),
            capacity: serde_json::json!({}),
            last_heartbeat: Some(chrono::Utc::now()),
            edge_public_key: None,
            compute_cidr: None,
            underlay_address: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    fn make_node_with_capacity(id: i32, name: &str, capacity: serde_json::Value) -> nodes::Model {
        let mut node = make_node(id, name);
        node.capacity = capacity;
        node
    }

    fn make_node_with_labels(id: i32, name: &str, labels: serde_json::Value) -> nodes::Model {
        let mut node = make_node(id, name);
        node.labels = labels;
        node
    }

    #[tokio::test]
    async fn test_schedule_no_nodes_returns_local() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<nodes::Model>::new()])
            .into_connection();
        let node_service = Arc::new(NodeService::new(Arc::new(db)));
        let scheduler = NodeScheduler::new(node_service);

        let assignments = scheduler
            .schedule_replicas(3, None, None, false)
            .await
            .unwrap();
        assert_eq!(assignments.len(), 3);
        for a in &assignments {
            assert!(a.is_local());
        }
    }

    #[tokio::test]
    async fn test_schedule_round_robin_with_no_capacity_data() {
        // Nodes without capacity data → falls back to round-robin even with LeastLoaded
        let node_a = make_node(1, "worker-a");
        let node_b = make_node(2, "worker-b");

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![node_a, node_b]])
            .into_connection();
        let node_service = Arc::new(NodeService::new(Arc::new(db)));
        let scheduler = NodeScheduler::new(node_service);

        let assignments = scheduler
            .schedule_replicas(6, None, None, false)
            .await
            .unwrap();
        assert_eq!(assignments.len(), 6);

        // Pool is [Local, worker-a, worker-b] → round-robin: Local, A, B, Local, A, B
        assert!(assignments[0].is_local(), "First replica should be Local");
        match &assignments[1] {
            NodeAssignment::Remote { node_id, .. } => assert_eq!(*node_id, 1),
            _ => panic!("Expected Remote assignment for worker-a"),
        }
        match &assignments[2] {
            NodeAssignment::Remote { node_id, .. } => assert_eq!(*node_id, 2),
            _ => panic!("Expected Remote assignment for worker-b"),
        }
        assert!(assignments[3].is_local(), "Fourth replica should be Local");
    }

    #[tokio::test]
    async fn test_schedule_explicit_round_robin() {
        let node_a = make_node(1, "worker-a");
        let node_b = make_node(2, "worker-b");

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![node_a, node_b]])
            .into_connection();
        let node_service = Arc::new(NodeService::new(Arc::new(db)));
        let scheduler =
            NodeScheduler::new(node_service).with_strategy(SchedulingStrategy::RoundRobin);

        let assignments = scheduler
            .schedule_replicas(6, None, None, false)
            .await
            .unwrap();
        assert_eq!(assignments.len(), 6);
        assert!(assignments[0].is_local());
        assert_eq!(assignments[1].node_id(), Some(1));
        assert_eq!(assignments[2].node_id(), Some(2));
        assert!(assignments[3].is_local());
        assert_eq!(assignments[4].node_id(), Some(1));
        assert_eq!(assignments[5].node_id(), Some(2));
    }

    #[tokio::test]
    async fn test_schedule_two_replicas_one_worker_splits_local_and_remote() {
        let node_a = make_node(1, "worker-a");

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![node_a]])
            .into_connection();
        let node_service = Arc::new(NodeService::new(Arc::new(db)));
        let scheduler = NodeScheduler::new(node_service);

        let assignments = scheduler
            .schedule_replicas(2, None, None, false)
            .await
            .unwrap();
        assert_eq!(assignments.len(), 2);

        // No capacity data → round-robin fallback: Local, worker-a
        assert!(assignments[0].is_local(), "First replica should be Local");
        match &assignments[1] {
            NodeAssignment::Remote { node_id, .. } => assert_eq!(*node_id, 1),
            _ => panic!("Expected Remote assignment for worker-a"),
        }
    }

    #[tokio::test]
    async fn test_schedule_single() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<nodes::Model>::new()])
            .into_connection();
        let node_service = Arc::new(NodeService::new(Arc::new(db)));
        let scheduler = NodeScheduler::new(node_service);

        let assignment = scheduler.schedule_single(None).await.unwrap();
        assert!(assignment.is_local());
    }

    #[tokio::test]
    async fn test_schedule_with_target_nodes_filters() {
        let node_a = make_node(1, "worker-a");
        let node_b = make_node(2, "worker-b");
        let node_c = make_node(3, "worker-c");

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![node_a, node_b, node_c]])
            .into_connection();
        let node_service = Arc::new(NodeService::new(Arc::new(db)));
        let scheduler = NodeScheduler::new(node_service);

        let target_ids = vec![1, 3];
        let assignments = scheduler
            .schedule_replicas(6, None, Some(&target_ids), false)
            .await
            .unwrap();
        assert_eq!(assignments.len(), 6);

        for assignment in &assignments {
            match assignment {
                NodeAssignment::Remote { node_id, .. } => {
                    assert!(
                        *node_id == 1 || *node_id == 3,
                        "Expected node 1 or 3, got {}",
                        node_id
                    );
                }
                NodeAssignment::Local => {}
            }
        }
    }

    #[tokio::test]
    async fn test_schedule_with_target_nodes_none_active_falls_back_to_local() {
        let node_a = make_node(1, "worker-a");

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![node_a]])
            .into_connection();
        let node_service = Arc::new(NodeService::new(Arc::new(db)));
        let scheduler = NodeScheduler::new(node_service);

        let target_ids = vec![99];
        let assignments = scheduler
            .schedule_replicas(2, None, Some(&target_ids), false)
            .await
            .unwrap();
        assert_eq!(assignments.len(), 2);
        for a in &assignments {
            assert!(a.is_local());
        }
    }

    #[test]
    fn test_node_assignment_accessors() {
        let local = NodeAssignment::Local;
        assert!(local.is_local());
        assert!(local.node_id().is_none());
        assert!(local.private_address().is_none());

        let remote = NodeAssignment::Remote {
            node_id: 5,
            node_name: "worker-5".to_string(),
            address: "https://10.100.0.5:3100".to_string(),
            private_address: "10.100.0.5".to_string(),
        };
        assert!(!remote.is_local());
        assert_eq!(remote.node_id(), Some(5));
        assert_eq!(remote.private_address(), Some("10.100.0.5"));
    }

    // --- Load score unit tests ---

    #[test]
    fn test_compute_load_score_cpu_and_memory() {
        let capacity = serde_json::json!({
            "cpu_percent": 40.0,
            "memory_used_bytes": 500_000_000.0,
            "memory_total_bytes": 1_000_000_000.0,
        });
        let score = compute_load_score(&capacity).unwrap();
        // CPU: 40%, Memory: 50% → (40*0.6 + 50*0.4) / 100 = (24+20)/100 = 0.44
        assert!((score - 0.44).abs() < 0.01, "Expected ~0.44, got {}", score);
    }

    #[test]
    fn test_compute_load_score_cpu_only() {
        let capacity = serde_json::json!({"cpu_percent": 80.0});
        let score = compute_load_score(&capacity).unwrap();
        assert!((score - 0.80).abs() < 0.01);
    }

    #[test]
    fn test_compute_load_score_memory_only() {
        let capacity = serde_json::json!({
            "memory_used_bytes": 750_000_000.0,
            "memory_total_bytes": 1_000_000_000.0,
        });
        let score = compute_load_score(&capacity).unwrap();
        assert!((score - 0.75).abs() < 0.01);
    }

    #[test]
    fn test_compute_load_score_empty_capacity() {
        let capacity = serde_json::json!({});
        assert!(compute_load_score(&capacity).is_none());
    }

    #[test]
    fn test_compute_load_score_null_capacity() {
        let capacity = serde_json::Value::Null;
        assert!(compute_load_score(&capacity).is_none());
    }

    // --- Least-loaded scheduling tests ---

    #[tokio::test]
    async fn test_least_loaded_prefers_idle_node() {
        // worker-a: 80% CPU, worker-b: 20% CPU → should prefer worker-b
        let node_a = make_node_with_capacity(
            1,
            "worker-a",
            serde_json::json!({"cpu_percent": 80.0, "memory_used_bytes": 800_000_000.0, "memory_total_bytes": 1_000_000_000.0}),
        );
        let node_b = make_node_with_capacity(
            2,
            "worker-b",
            serde_json::json!({"cpu_percent": 20.0, "memory_used_bytes": 200_000_000.0, "memory_total_bytes": 1_000_000_000.0}),
        );

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![node_a, node_b]])
            .into_connection();
        let node_service = Arc::new(NodeService::new(Arc::new(db)));
        let scheduler = NodeScheduler::new(node_service);

        let assignments = scheduler
            .schedule_replicas(1, None, None, false)
            .await
            .unwrap();
        assert_eq!(assignments.len(), 1);
        // worker-b (id=2) has score ~0.20, Local has score 0.5 (no data),
        // worker-a has score ~0.80 → first pick should be worker-b
        assert_eq!(
            assignments[0].node_id(),
            Some(2),
            "Should pick the least loaded node (worker-b)"
        );
    }

    #[tokio::test]
    async fn test_least_loaded_spreads_replicas() {
        // Two nodes with similar load — replicas should spread across both
        let node_a = make_node_with_capacity(
            1,
            "worker-a",
            serde_json::json!({"cpu_percent": 30.0, "memory_used_bytes": 300_000_000.0, "memory_total_bytes": 1_000_000_000.0}),
        );
        let node_b = make_node_with_capacity(
            2,
            "worker-b",
            serde_json::json!({"cpu_percent": 30.0, "memory_used_bytes": 300_000_000.0, "memory_total_bytes": 1_000_000_000.0}),
        );

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![node_a, node_b]])
            .into_connection();
        let node_service = Arc::new(NodeService::new(Arc::new(db)));
        let scheduler = NodeScheduler::new(node_service);

        let assignments = scheduler
            .schedule_replicas(3, None, None, false)
            .await
            .unwrap();
        assert_eq!(assignments.len(), 3);

        // Both workers have score ~0.30, Local has score 0.5
        // First two picks should be the two workers, third should be something else
        let remote_count = assignments.iter().filter(|a| !a.is_local()).count();
        let local_count = assignments.iter().filter(|a| a.is_local()).count();
        assert!(
            remote_count >= 2,
            "Should assign at least 2 replicas to workers"
        );
        assert!(local_count >= 1 || remote_count == 3, "Spread across pool");
    }

    #[tokio::test]
    async fn test_least_loaded_avoids_overloaded_node() {
        // One node at 95% → should avoid it
        let node_heavy = make_node_with_capacity(
            1,
            "worker-heavy",
            serde_json::json!({"cpu_percent": 95.0, "memory_used_bytes": 950_000_000.0, "memory_total_bytes": 1_000_000_000.0}),
        );
        let node_light = make_node_with_capacity(
            2,
            "worker-light",
            serde_json::json!({"cpu_percent": 10.0, "memory_used_bytes": 100_000_000.0, "memory_total_bytes": 1_000_000_000.0}),
        );

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![node_heavy, node_light]])
            .into_connection();
        let node_service = Arc::new(NodeService::new(Arc::new(db)));
        let scheduler = NodeScheduler::new(node_service);

        let assignments = scheduler
            .schedule_replicas(2, None, None, false)
            .await
            .unwrap();

        // First pick: worker-light (0.10), second pick: Local (0.5) or worker-light bumped
        // worker-heavy (0.95) should not be first or second pick
        assert_eq!(
            assignments[0].node_id(),
            Some(2),
            "First pick should be the light node"
        );
        assert_ne!(
            assignments[1].node_id(),
            Some(1),
            "Second pick should not be the heavy node"
        );
    }

    // --- Label matching unit tests ---

    #[test]
    fn test_label_match_single_key_exact() {
        let node_labels = serde_json::json!({"region": "us", "gpu": "true"});
        let selector = serde_json::json!({"region": "us"});
        assert!(node_matches_labels(&node_labels, &selector));
    }

    #[test]
    fn test_label_match_single_key_no_match() {
        let node_labels = serde_json::json!({"region": "eu"});
        let selector = serde_json::json!({"region": "us"});
        assert!(!node_matches_labels(&node_labels, &selector));
    }

    #[test]
    fn test_label_match_or_within_same_key() {
        let node_labels = serde_json::json!({"region": "asia"});
        let selector = serde_json::json!({"region": ["us", "asia"]});
        assert!(node_matches_labels(&node_labels, &selector));
    }

    #[test]
    fn test_label_match_or_within_same_key_no_match() {
        let node_labels = serde_json::json!({"region": "eu"});
        let selector = serde_json::json!({"region": ["us", "asia"]});
        assert!(!node_matches_labels(&node_labels, &selector));
    }

    #[test]
    fn test_label_match_and_across_keys() {
        let node_labels = serde_json::json!({"region": "us", "gpu": "true"});
        let selector = serde_json::json!({"region": "us", "gpu": "true"});
        assert!(node_matches_labels(&node_labels, &selector));
    }

    #[test]
    fn test_label_match_and_across_keys_partial_fail() {
        let node_labels = serde_json::json!({"region": "us", "gpu": "false"});
        let selector = serde_json::json!({"region": "us", "gpu": "true"});
        assert!(!node_matches_labels(&node_labels, &selector));
    }

    #[test]
    fn test_label_match_or_and_combined() {
        let selector = serde_json::json!({"region": ["us", "asia"], "gpu": "true"});

        let node1 = serde_json::json!({"region": "asia", "gpu": "true"});
        assert!(node_matches_labels(&node1, &selector));

        let node2 = serde_json::json!({"region": "us", "gpu": "true"});
        assert!(node_matches_labels(&node2, &selector));

        let node3 = serde_json::json!({"region": "eu", "gpu": "true"});
        assert!(!node_matches_labels(&node3, &selector));

        let node4 = serde_json::json!({"region": "us", "gpu": "false"});
        assert!(!node_matches_labels(&node4, &selector));
    }

    #[test]
    fn test_label_match_missing_key_on_node() {
        let node_labels = serde_json::json!({"region": "us"});
        let selector = serde_json::json!({"region": "us", "gpu": "true"});
        assert!(!node_matches_labels(&node_labels, &selector));
    }

    #[test]
    fn test_label_match_empty_selector_matches_all() {
        let node_labels = serde_json::json!({"region": "us"});
        let selector = serde_json::json!({});
        assert!(node_matches_labels(&node_labels, &selector));
    }

    #[test]
    fn test_label_match_empty_node_labels_no_match() {
        let node_labels = serde_json::json!({});
        let selector = serde_json::json!({"region": "us"});
        assert!(!node_matches_labels(&node_labels, &selector));
    }

    #[test]
    fn test_label_match_null_node_labels_empty_selector() {
        let node_labels = serde_json::Value::Null;
        let selector = serde_json::json!({});
        assert!(node_matches_labels(&node_labels, &selector));
    }

    #[tokio::test]
    async fn test_schedule_with_labels_filters_nodes() {
        let node_us = make_node_with_labels(1, "us-worker", serde_json::json!({"region": "us"}));
        let node_eu = make_node_with_labels(2, "eu-worker", serde_json::json!({"region": "eu"}));
        let node_asia =
            make_node_with_labels(3, "asia-worker", serde_json::json!({"region": "asia"}));

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![node_us, node_eu, node_asia]])
            .into_connection();
        let node_service = Arc::new(NodeService::new(Arc::new(db)));
        let scheduler = NodeScheduler::new(node_service);

        let labels = serde_json::json!({"region": ["us", "asia"]});
        let assignments = scheduler
            .schedule_replicas(4, Some(&labels), None, false)
            .await
            .unwrap();
        assert_eq!(assignments.len(), 4);

        for assignment in &assignments {
            if let NodeAssignment::Remote { node_id, .. } = assignment {
                assert!(
                    *node_id == 1 || *node_id == 3,
                    "Expected node 1 (us) or 3 (asia), got {}",
                    node_id
                );
            }
        }
    }

    #[tokio::test]
    async fn test_schedule_with_labels_no_match_falls_back_to_local() {
        let node_eu = make_node_with_labels(1, "eu-worker", serde_json::json!({"region": "eu"}));

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![node_eu]])
            .into_connection();
        let node_service = Arc::new(NodeService::new(Arc::new(db)));
        let scheduler = NodeScheduler::new(node_service);

        let labels = serde_json::json!({"region": "us"});
        let assignments = scheduler
            .schedule_replicas(2, Some(&labels), None, false)
            .await
            .unwrap();
        assert_eq!(assignments.len(), 2);
        for a in &assignments {
            assert!(
                a.is_local(),
                "Should fall back to local when no labels match"
            );
        }
    }

    // --- Anti-affinity tests ---

    #[tokio::test]
    async fn test_anti_affinity_spreads_replicas_across_nodes() {
        // 3 nodes + local = 4 slots. 4 replicas should each land on a different slot.
        let node_a = make_node(1, "worker-a");
        let node_b = make_node(2, "worker-b");
        let node_c = make_node(3, "worker-c");

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![node_a, node_b, node_c]])
            .into_connection();
        let node_service = Arc::new(NodeService::new(Arc::new(db)));
        let scheduler =
            NodeScheduler::new(node_service).with_strategy(SchedulingStrategy::RoundRobin);

        let assignments = scheduler
            .schedule_replicas(4, None, None, true)
            .await
            .unwrap();
        assert_eq!(assignments.len(), 4);

        // With anti-affinity and 4 slots, each replica should be on a unique node
        let mut seen_node_ids: Vec<Option<i32>> = assignments.iter().map(|a| a.node_id()).collect();
        // Local has node_id() = None, remote has Some(id)
        seen_node_ids.sort();
        seen_node_ids.dedup();
        assert_eq!(
            seen_node_ids.len(),
            4,
            "All 4 replicas should be on different nodes, got {:?}",
            assignments.iter().map(|a| a.node_id()).collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn test_anti_affinity_wraps_around_when_more_replicas_than_nodes() {
        // 2 nodes + local = 3 slots. 5 replicas: first 3 spread, then wraps.
        let node_a = make_node(1, "worker-a");
        let node_b = make_node(2, "worker-b");

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![node_a, node_b]])
            .into_connection();
        let node_service = Arc::new(NodeService::new(Arc::new(db)));
        let scheduler =
            NodeScheduler::new(node_service).with_strategy(SchedulingStrategy::RoundRobin);

        let assignments = scheduler
            .schedule_replicas(5, None, None, true)
            .await
            .unwrap();
        assert_eq!(assignments.len(), 5);

        // First 3 should be unique (Local, node-1, node-2)
        let first_three: Vec<Option<i32>> = assignments[..3].iter().map(|a| a.node_id()).collect();
        let mut unique = first_three.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(
            unique.len(),
            3,
            "First 3 replicas should each be on different nodes"
        );

        // All 3 slots should be used at least once
        let all_ids: Vec<Option<i32>> = assignments.iter().map(|a| a.node_id()).collect();
        assert!(all_ids.contains(&None), "Local should be used");
        assert!(all_ids.contains(&Some(1)), "Node 1 should be used");
        assert!(all_ids.contains(&Some(2)), "Node 2 should be used");
    }

    #[tokio::test]
    async fn test_anti_affinity_with_least_loaded_spreads() {
        // Two nodes with different load: anti-affinity should still spread across all.
        let node_light = make_node_with_capacity(
            1,
            "worker-light",
            serde_json::json!({"cpu_percent": 10.0, "memory_used_bytes": 100_000_000.0, "memory_total_bytes": 1_000_000_000.0}),
        );
        let node_heavy = make_node_with_capacity(
            2,
            "worker-heavy",
            serde_json::json!({"cpu_percent": 80.0, "memory_used_bytes": 800_000_000.0, "memory_total_bytes": 1_000_000_000.0}),
        );

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![node_light, node_heavy]])
            .into_connection();
        let node_service = Arc::new(NodeService::new(Arc::new(db)));
        let scheduler = NodeScheduler::new(node_service); // LeastLoaded default

        let assignments = scheduler
            .schedule_replicas(3, None, None, true)
            .await
            .unwrap();
        assert_eq!(assignments.len(), 3);

        // With anti-affinity, all 3 slots should be used even though worker-heavy is loaded
        let ids: Vec<Option<i32>> = assignments.iter().map(|a| a.node_id()).collect();
        let mut unique = ids.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(
            unique.len(),
            3,
            "Anti-affinity should spread across all 3 nodes, got {:?}",
            ids
        );
    }

    #[tokio::test]
    async fn test_anti_affinity_disabled_allows_stacking() {
        // Without anti-affinity, least-loaded can stack replicas on the same node
        let node_light = make_node_with_capacity(
            1,
            "worker-light",
            serde_json::json!({"cpu_percent": 5.0, "memory_used_bytes": 50_000_000.0, "memory_total_bytes": 1_000_000_000.0}),
        );
        let node_heavy = make_node_with_capacity(
            2,
            "worker-heavy",
            serde_json::json!({"cpu_percent": 95.0, "memory_used_bytes": 950_000_000.0, "memory_total_bytes": 1_000_000_000.0}),
        );

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![node_light, node_heavy]])
            .into_connection();
        let node_service = Arc::new(NodeService::new(Arc::new(db)));
        let scheduler = NodeScheduler::new(node_service); // LeastLoaded

        let assignments = scheduler
            .schedule_replicas(3, None, None, false)
            .await
            .unwrap();
        assert_eq!(assignments.len(), 3);

        // Without anti-affinity, all 3 may end up on worker-light (lightest node)
        // At minimum, first two should pick worker-light before bump pushes elsewhere
        assert_eq!(
            assignments[0].node_id(),
            Some(1),
            "First replica should go to lightest node"
        );
    }

    #[tokio::test]
    async fn test_anti_affinity_single_node_no_effect() {
        // Only 1 remote node + local = 2 slots. 4 replicas.
        let node = make_node(1, "worker-a");

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![node]])
            .into_connection();
        let node_service = Arc::new(NodeService::new(Arc::new(db)));
        let scheduler =
            NodeScheduler::new(node_service).with_strategy(SchedulingStrategy::RoundRobin);

        let assignments = scheduler
            .schedule_replicas(4, None, None, true)
            .await
            .unwrap();
        assert_eq!(assignments.len(), 4);

        // Both slots should be used
        let local_count = assignments.iter().filter(|a| a.is_local()).count();
        let remote_count = assignments.iter().filter(|a| !a.is_local()).count();
        assert_eq!(local_count, 2, "Should have 2 local replicas");
        assert_eq!(remote_count, 2, "Should have 2 remote replicas");
    }

    #[tokio::test]
    async fn test_anti_affinity_no_nodes_returns_all_local() {
        // No remote nodes → all local regardless of anti-affinity
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<nodes::Model>::new()])
            .into_connection();
        let node_service = Arc::new(NodeService::new(Arc::new(db)));
        let scheduler = NodeScheduler::new(node_service);

        let assignments = scheduler
            .schedule_replicas(3, None, None, true)
            .await
            .unwrap();
        assert_eq!(assignments.len(), 3);
        for a in &assignments {
            assert!(a.is_local());
        }
    }

    // --- Resource hard limit tests ---

    #[tokio::test]
    async fn test_max_load_threshold_excludes_overloaded_node() {
        // worker-heavy at 95% should be excluded (default threshold 90%)
        let node_heavy = make_node_with_capacity(
            1,
            "worker-heavy",
            serde_json::json!({"cpu_percent": 95.0, "memory_used_bytes": 950_000_000.0, "memory_total_bytes": 1_000_000_000.0}),
        );
        let node_light = make_node_with_capacity(
            2,
            "worker-light",
            serde_json::json!({"cpu_percent": 20.0, "memory_used_bytes": 200_000_000.0, "memory_total_bytes": 1_000_000_000.0}),
        );

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![node_heavy, node_light]])
            .into_connection();
        let node_service = Arc::new(NodeService::new(Arc::new(db)));
        let scheduler = NodeScheduler::new(node_service); // default threshold 0.90

        let assignments = scheduler
            .schedule_replicas(3, None, None, false)
            .await
            .unwrap();
        assert_eq!(assignments.len(), 3);

        // worker-heavy (score ~0.95) should never be picked
        for a in &assignments {
            assert_ne!(
                a.node_id(),
                Some(1),
                "Overloaded node should be excluded from scheduling"
            );
        }
    }

    #[tokio::test]
    async fn test_max_load_threshold_all_overloaded_falls_back() {
        // Both nodes at >90% → should still schedule (best-effort fallback)
        let node_a = make_node_with_capacity(
            1,
            "worker-a",
            serde_json::json!({"cpu_percent": 92.0, "memory_used_bytes": 920_000_000.0, "memory_total_bytes": 1_000_000_000.0}),
        );
        let node_b = make_node_with_capacity(
            2,
            "worker-b",
            serde_json::json!({"cpu_percent": 95.0, "memory_used_bytes": 950_000_000.0, "memory_total_bytes": 1_000_000_000.0}),
        );

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![node_a, node_b]])
            .into_connection();
        let node_service = Arc::new(NodeService::new(Arc::new(db)));
        let scheduler = NodeScheduler::new(node_service);

        let assignments = scheduler
            .schedule_replicas(2, None, None, false)
            .await
            .unwrap();
        assert_eq!(assignments.len(), 2);

        // All nodes exceed the threshold so the limit is relaxed (best-effort).
        // Pool is [Local(0.5), worker-a(~0.92), worker-b(~0.95)].
        // LeastLoaded picks Local first (0.5), bumps to 0.83. Next pick:
        // Local(0.83) < worker-a(0.92) < worker-b(0.95) → Local again.
        // Key assertion: scheduling still proceeds even when all nodes are
        // overloaded (the overloaded heavy node is not scheduled for 2 replicas).
        assert_ne!(
            assignments[0].node_id(),
            Some(2),
            "Should not pick the most overloaded node (worker-b)"
        );
        assert_ne!(
            assignments[1].node_id(),
            Some(2),
            "Should not pick the most overloaded node (worker-b)"
        );
    }

    #[tokio::test]
    async fn test_max_load_threshold_custom_value() {
        // Custom threshold at 50% — only the light node should pass
        let node_moderate = make_node_with_capacity(
            1,
            "worker-moderate",
            serde_json::json!({"cpu_percent": 60.0, "memory_used_bytes": 600_000_000.0, "memory_total_bytes": 1_000_000_000.0}),
        );
        let node_light = make_node_with_capacity(
            2,
            "worker-light",
            serde_json::json!({"cpu_percent": 30.0, "memory_used_bytes": 300_000_000.0, "memory_total_bytes": 1_000_000_000.0}),
        );

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![node_moderate, node_light]])
            .into_connection();
        let node_service = Arc::new(NodeService::new(Arc::new(db)));
        let scheduler = NodeScheduler::new(node_service).with_max_load_threshold(0.50);

        let assignments = scheduler
            .schedule_replicas(2, None, None, false)
            .await
            .unwrap();
        assert_eq!(assignments.len(), 2);

        // worker-moderate (score ~0.60) should be excluded with 50% threshold
        // Only Local (no data → eligible) and worker-light (~0.30) remain
        for a in &assignments {
            assert_ne!(
                a.node_id(),
                Some(1),
                "Moderate node should be excluded at 50% threshold"
            );
        }
    }

    #[tokio::test]
    async fn test_max_load_threshold_no_capacity_data_always_eligible() {
        // Nodes without capacity data should always be eligible
        let node_no_data = make_node(1, "worker-no-data"); // no capacity
        let node_heavy = make_node_with_capacity(
            2,
            "worker-heavy",
            serde_json::json!({"cpu_percent": 95.0, "memory_used_bytes": 950_000_000.0, "memory_total_bytes": 1_000_000_000.0}),
        );

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![node_no_data, node_heavy]])
            .into_connection();
        let node_service = Arc::new(NodeService::new(Arc::new(db)));
        let scheduler = NodeScheduler::new(node_service);

        let assignments = scheduler
            .schedule_replicas(2, None, None, false)
            .await
            .unwrap();
        assert_eq!(assignments.len(), 2);

        // worker-heavy should be excluded, but worker-no-data and Local should be eligible
        for a in &assignments {
            assert_ne!(
                a.node_id(),
                Some(2),
                "Heavy node should be excluded even when no-data node exists"
            );
        }
    }

    // --- Rolling update exclusion tests ---

    #[tokio::test]
    async fn test_exclude_nodes_during_rolling_update() {
        // 3 worker nodes. Node 1 and 2 have existing containers → should be excluded.
        let node_a = make_node(1, "worker-a");
        let node_b = make_node(2, "worker-b");
        let node_c = make_node(3, "worker-c");

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![node_a, node_b, node_c]])
            .into_connection();
        let node_service = Arc::new(NodeService::new(Arc::new(db)));
        let scheduler =
            NodeScheduler::new(node_service).with_strategy(SchedulingStrategy::RoundRobin);

        let exclude = vec![1, 2]; // nodes with outgoing containers
        let assignments = scheduler
            .schedule_replicas_excluding(2, None, None, true, &exclude)
            .await
            .unwrap();
        assert_eq!(assignments.len(), 2);

        // Should only use Local and worker-c (node 3)
        for a in &assignments {
            assert!(
                a.is_local() || a.node_id() == Some(3),
                "Expected Local or node 3, got node {:?}",
                a.node_id()
            );
        }
    }

    #[tokio::test]
    async fn test_exclude_nodes_relaxed_when_all_excluded() {
        // 2 worker nodes, both excluded → should relax and still schedule
        let node_a = make_node(1, "worker-a");
        let node_b = make_node(2, "worker-b");

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![node_a, node_b]])
            .into_connection();
        let node_service = Arc::new(NodeService::new(Arc::new(db)));
        let scheduler =
            NodeScheduler::new(node_service).with_strategy(SchedulingStrategy::RoundRobin);

        let exclude = vec![1, 2]; // exclude all remote nodes
        let assignments = scheduler
            .schedule_replicas_excluding(3, None, None, true, &exclude)
            .await
            .unwrap();
        assert_eq!(assignments.len(), 3);

        // Local is never excluded, so even with all remote excluded, we still
        // get Local. Since exclusion was relaxed, remote nodes also become available.
        assert!(!assignments.is_empty());
    }

    #[tokio::test]
    async fn test_exclude_nodes_ignored_without_anti_affinity() {
        // When anti-affinity is disabled, exclude_node_ids should have no effect
        let node_a = make_node(1, "worker-a");
        let node_b = make_node(2, "worker-b");

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![node_a, node_b]])
            .into_connection();
        let node_service = Arc::new(NodeService::new(Arc::new(db)));
        let scheduler =
            NodeScheduler::new(node_service).with_strategy(SchedulingStrategy::RoundRobin);

        let exclude = vec![1, 2];
        let assignments = scheduler
            .schedule_replicas_excluding(6, None, None, false, &exclude)
            .await
            .unwrap();
        assert_eq!(assignments.len(), 6);

        // With anti-affinity disabled, excluded nodes should still be used
        let node_ids: Vec<Option<i32>> = assignments.iter().map(|a| a.node_id()).collect();
        assert!(
            node_ids.contains(&Some(1)) || node_ids.contains(&Some(2)),
            "Excluded nodes should still be used without anti-affinity"
        );
    }
}
