//! Node management service — CRUD operations for the `nodes` table.

use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter,
    QueryOrder,
};
use std::sync::Arc;
use thiserror::Error;

use temps_entities::{deployment_containers, deployments, nodes};

#[derive(Error, Debug)]
pub enum NodeError {
    #[error("Node '{name}' not found")]
    NotFound { name: String },

    #[error("Node with id {node_id} not found")]
    NotFoundById { node_id: i32 },

    #[error("Node '{name}' already exists")]
    AlreadyExists { name: String },

    #[error("Invalid node configuration: {message}")]
    Validation { message: String },

    #[error("Node '{name}' is currently active; re-registering a different identity (token/address/WireGuard key) requires proof of the current token, or the node must be drained/removed first")]
    IdentityConflict { name: String },

    #[error("Database error: {0}")]
    Database(#[from] sea_orm::DbErr),
}

/// Request to register a new worker node.
#[derive(Debug, Clone)]
pub struct RegisterNodeRequest {
    pub name: String,
    pub token_hash: String,
    /// Encrypted plaintext token for control plane → agent auth
    pub token_encrypted: Option<String>,
    pub address: String,
    pub private_address: String,
    pub public_endpoint: Option<String>,
    pub wg_public_key: Option<String>,
    pub role: String,
    pub labels: serde_json::Value,
    /// X25519 public key for ECIES certificate encryption (edge nodes only)
    pub edge_public_key: Option<String>,
    /// SHA-256 hash of the node's *current* token, supplied to prove possession
    /// when re-registering (changing identity) a node that already exists.
    /// `None` for first-time registration or when no proof is offered.
    /// (ADR-020 WS-1.2 / enroll-1.)
    pub prior_token_hash: Option<String>,
}

/// Request to update a node's heartbeat.
#[derive(Debug, Clone)]
pub struct HeartbeatRequest {
    pub capacity: serde_json::Value,
    /// Updated labels from the agent (allows runtime label changes without re-registration).
    pub labels: Option<serde_json::Value>,
}

/// A node whose last heartbeat is within this window (and still marked
/// "active") is considered live, and its identity may not be silently rebound
/// by a re-registration. Mirrors the health-check stale threshold.
const NODE_LIVE_THRESHOLD_SECS: i64 = 90;

/// Constant-time comparison of two equal-purpose byte slices (SHA-256 hex
/// token hashes) to avoid leaking a match via timing.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

pub struct NodeService {
    db: Arc<DatabaseConnection>,
}

impl NodeService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    /// Register a new node in the cluster.
    pub async fn register(&self, request: RegisterNodeRequest) -> Result<nodes::Model, NodeError> {
        if request.name.is_empty() {
            return Err(NodeError::Validation {
                message: "Node name cannot be empty".into(),
            });
        }
        // Restrict to a DNS-label-style charset. The name is surfaced in logs
        // and injected into every container's `TEMPS_NODE_NAME` env var, so
        // reject shell metacharacters / newlines / control chars defensively.
        if request.name.len() > 63
            || !request
                .name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.' || b == b'_')
        {
            return Err(NodeError::Validation {
                message: "Node name must be <=63 chars of letters, digits, '-', '.', or '_'".into(),
            });
        }

        if request.address.is_empty() {
            return Err(NodeError::Validation {
                message: "Node address cannot be empty".into(),
            });
        }

        // Check for existing node with the same name
        let existing = nodes::Entity::find()
            .filter(nodes::Column::Name.eq(&request.name))
            .one(self.db.as_ref())
            .await?;

        if let Some(existing_node) = existing {
            // Identity-change guard (ADR-020 WS-1.2 / enroll-1).
            //
            // Registration resolves an existing node purely by name. Without
            // this guard, anyone holding the shared cluster join token could
            // POST a register for an existing node's name with their own token,
            // address, and WireGuard key and silently rebind that node — the
            // control plane would then dial the attacker's address to deploy,
            // exec, and hand out secrets. We refuse to overwrite an existing
            // node's identity UNLESS either (a) the caller proves possession of
            // the node's current token, or (b) the node is not currently live
            // (an operator legitimately re-provisioning a dead node). A
            // genuinely live, healthy node is never simultaneously re-joining.
            let identity_change = existing_node.token_hash != request.token_hash
                || existing_node.address != request.address
                || existing_node.private_address != request.private_address
                || existing_node.wg_public_key != request.wg_public_key;

            if identity_change {
                let proven = match &request.prior_token_hash {
                    Some(h) => constant_time_eq(h.as_bytes(), existing_node.token_hash.as_bytes()),
                    None => false,
                };
                let is_live = existing_node.status == "active"
                    && existing_node
                        .last_heartbeat
                        .map(|h| (chrono::Utc::now() - h).num_seconds() < NODE_LIVE_THRESHOLD_SECS)
                        .unwrap_or(false);

                if is_live && !proven {
                    tracing::warn!(
                        node_id = existing_node.id,
                        node_name = %existing_node.name,
                        old_address = %existing_node.address,
                        new_address = %request.address,
                        "Rejected node re-registration: identity change on a live node with no proof of the current token (possible takeover via shared join token)"
                    );
                    return Err(NodeError::IdentityConflict { name: request.name });
                }

                tracing::warn!(
                    node_id = existing_node.id,
                    node_name = %existing_node.name,
                    proven,
                    is_live,
                    old_address = %existing_node.address,
                    new_address = %request.address,
                    "Node identity re-bound (token/address/WireGuard key changed)"
                );
            }

            // Reconnection: update the existing node and set it back to active
            let mut active: nodes::ActiveModel = existing_node.into();
            active.token_hash = Set(request.token_hash);
            active.token_encrypted = Set(request.token_encrypted.clone());
            active.address = Set(request.address);
            active.private_address = Set(request.private_address);
            active.public_endpoint = Set(request.public_endpoint);
            active.wg_public_key = Set(request.wg_public_key);
            active.labels = Set(request.labels);
            active.edge_public_key = Set(request.edge_public_key);
            active.status = Set("active".to_string());
            active.last_heartbeat = Set(Some(chrono::Utc::now()));

            let node = active.update(self.db.as_ref()).await?;

            tracing::info!(
                node_id = node.id,
                node_name = %node.name,
                address = %node.address,
                "Node reconnected"
            );

            return Ok(node);
        }

        let model = nodes::ActiveModel {
            name: Set(request.name),
            token_hash: Set(request.token_hash),
            token_encrypted: Set(request.token_encrypted),
            address: Set(request.address),
            private_address: Set(request.private_address),
            public_endpoint: Set(request.public_endpoint),
            wg_public_key: Set(request.wg_public_key),
            role: Set(request.role),
            status: Set("active".to_string()),
            labels: Set(request.labels),
            capacity: Set(serde_json::json!({})),
            last_heartbeat: Set(Some(chrono::Utc::now())),
            edge_public_key: Set(request.edge_public_key),
            ..Default::default()
        };

        let node = model.insert(self.db.as_ref()).await?;

        tracing::info!(
            node_id = node.id,
            node_name = %node.name,
            address = %node.address,
            private_address = %node.private_address,
            "Node registered"
        );

        Ok(node)
    }

    /// Update a node's heartbeat timestamp and capacity metrics.
    pub async fn heartbeat(
        &self,
        node_id: i32,
        request: HeartbeatRequest,
    ) -> Result<(), NodeError> {
        let node = nodes::Entity::find_by_id(node_id)
            .one(self.db.as_ref())
            .await?
            .ok_or(NodeError::NotFoundById { node_id })?;

        let mut active: nodes::ActiveModel = node.clone().into();
        active.last_heartbeat = Set(Some(chrono::Utc::now()));
        active.capacity = Set(request.capacity);
        // Only transition to "active" if the node was "offline" (reconnecting).
        // Preserve managed states like "draining" and "drained".
        if node.status == "offline" {
            active.status = Set("active".to_string());
        }
        if let Some(labels) = request.labels {
            active.labels = Set(labels);
        }
        active.update(self.db.as_ref()).await?;

        Ok(())
    }

    /// Get a node by its ID.
    pub async fn get_by_id(&self, node_id: i32) -> Result<nodes::Model, NodeError> {
        nodes::Entity::find_by_id(node_id)
            .one(self.db.as_ref())
            .await?
            .ok_or(NodeError::NotFoundById { node_id })
    }

    /// Authorization check for `get_s3_credentials` (ADR-020 WS-4.1 / analyst-1).
    ///
    /// A worker node may only receive the plaintext credentials for an S3 source
    /// it legitimately needs — i.e. a source used by a backup of a managed
    /// service that is actually hosted on this node (standalone placement via
    /// `external_services.node_id`, or a cluster member via
    /// `service_members.node_id`). Without this gate any single node token could
    /// enumerate `s3_source_id = 1..N` and exfiltrate every tenant's object-store
    /// access+secret keys.
    pub async fn is_authorized_for_s3_source(
        &self,
        node_id: i32,
        s3_source_id: i32,
    ) -> Result<bool, NodeError> {
        use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

        // EXISTS: a backup whose destination is `s3_source_id`, linked (via the
        // external_service_backups join) to a service placed on `node_id`.
        let sql = r#"
            SELECT 1
            FROM backups b
            JOIN external_service_backups esb ON esb.backup_id = b.id
            JOIN external_services es ON es.id = esb.service_id
            LEFT JOIN service_members sm ON sm.service_id = es.id
            WHERE b.s3_source_id = $1
              AND (es.node_id = $2 OR sm.node_id = $2)
            LIMIT 1
        "#;

        let stmt = Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            sql,
            [s3_source_id.into(), node_id.into()],
        );

        let row = self.db.query_one(stmt).await?;
        Ok(row.is_some())
    }

    /// Get a node by its name.
    pub async fn get_by_name(&self, name: &str) -> Result<nodes::Model, NodeError> {
        nodes::Entity::find()
            .filter(nodes::Column::Name.eq(name))
            .one(self.db.as_ref())
            .await?
            .ok_or(NodeError::NotFound {
                name: name.to_string(),
            })
    }

    /// List all nodes, ordered by name.
    pub async fn list_all(&self) -> Result<Vec<nodes::Model>, NodeError> {
        let nodes = nodes::Entity::find()
            .order_by_asc(nodes::Column::Name)
            .all(self.db.as_ref())
            .await?;
        Ok(nodes)
    }

    /// List only active nodes (heartbeat within the threshold).
    pub async fn list_active(
        &self,
        heartbeat_threshold_secs: i64,
    ) -> Result<Vec<nodes::Model>, NodeError> {
        let cutoff = chrono::Utc::now() - chrono::Duration::seconds(heartbeat_threshold_secs);

        let nodes = nodes::Entity::find()
            .filter(nodes::Column::Status.eq("active"))
            .filter(nodes::Column::LastHeartbeat.gte(cutoff))
            .order_by_asc(nodes::Column::Name)
            .all(self.db.as_ref())
            .await?;

        Ok(nodes)
    }

    /// Mark a node as offline.
    pub async fn mark_offline(&self, node_id: i32) -> Result<(), NodeError> {
        let node = nodes::Entity::find_by_id(node_id)
            .one(self.db.as_ref())
            .await?
            .ok_or(NodeError::NotFoundById { node_id })?;

        let mut active: nodes::ActiveModel = node.into();
        active.status = Set("offline".to_string());
        active.update(self.db.as_ref()).await?;

        tracing::warn!(node_id = node_id, "Node marked as offline");

        Ok(())
    }

    /// Mark a node as draining (no new deployments, existing continue).
    pub async fn mark_draining(&self, node_id: i32) -> Result<(), NodeError> {
        let node = nodes::Entity::find_by_id(node_id)
            .one(self.db.as_ref())
            .await?
            .ok_or(NodeError::NotFoundById { node_id })?;

        let mut active: nodes::ActiveModel = node.into();
        active.status = Set("draining".to_string());
        active.update(self.db.as_ref()).await?;

        tracing::info!(node_id = node_id, "Node marked as draining");

        Ok(())
    }

    /// Reactivate a drained or draining node so it can accept new deployments again.
    /// Only nodes in "draining" or "drained" status can be undrained.
    pub async fn mark_active(&self, node_id: i32) -> Result<(), NodeError> {
        let node = nodes::Entity::find_by_id(node_id)
            .one(self.db.as_ref())
            .await?
            .ok_or(NodeError::NotFoundById { node_id })?;

        if node.status != "draining" && node.status != "drained" {
            return Err(NodeError::Validation {
                message: format!(
                    "Cannot undrain node {}: current status is '{}', expected 'draining' or 'drained'",
                    node_id, node.status
                ),
            });
        }

        let mut active: nodes::ActiveModel = node.into();
        active.status = Set("active".to_string());
        active.update(self.db.as_ref()).await?;

        tracing::info!(node_id = node_id, "Node reactivated (undrained)");

        Ok(())
    }

    /// Check if a draining node has completed its drain (no remaining containers).
    /// If complete, transitions the node status from "draining" to "drained".
    /// Returns `true` if the drain is now complete.
    pub async fn check_drain_complete(&self, node_id: i32) -> Result<bool, NodeError> {
        let node = nodes::Entity::find_by_id(node_id)
            .one(self.db.as_ref())
            .await?
            .ok_or(NodeError::NotFoundById { node_id })?;

        if node.status != "draining" {
            return Ok(node.status == "drained");
        }

        let containers = self.list_containers_for_node(node_id).await?;

        if containers.is_empty() {
            let mut active: nodes::ActiveModel = node.into();
            active.status = Set("drained".to_string());
            active.update(self.db.as_ref()).await?;

            tracing::info!(
                node_id = node_id,
                "Node drain complete — all containers migrated, status set to drained"
            );
            return Ok(true);
        }

        tracing::debug!(
            node_id = node_id,
            remaining_containers = containers.len(),
            "Node drain in progress"
        );
        Ok(false)
    }

    /// Check all draining nodes for drain completion.
    /// Returns the list of node IDs that transitioned to "drained".
    pub async fn check_all_drains(&self) -> Result<Vec<i32>, NodeError> {
        let draining_nodes = nodes::Entity::find()
            .filter(nodes::Column::Status.eq("draining"))
            .all(self.db.as_ref())
            .await?;

        let mut completed = Vec::new();
        for node in draining_nodes {
            match self.check_drain_complete(node.id).await {
                Ok(true) => completed.push(node.id),
                Ok(false) => {}
                Err(e) => {
                    tracing::error!(node_id = node.id, "Failed to check drain completion: {}", e);
                }
            }
        }

        if !completed.is_empty() {
            tracing::info!(
                count = completed.len(),
                "Drain check: {} node(s) completed drain",
                completed.len()
            );
        }

        Ok(completed)
    }

    /// Remove a node from the cluster.
    pub async fn remove(&self, node_id: i32) -> Result<(), NodeError> {
        let result = nodes::Entity::delete_by_id(node_id)
            .exec(self.db.as_ref())
            .await?;

        if result.rows_affected == 0 {
            return Err(NodeError::NotFoundById { node_id });
        }

        tracing::info!(node_id = node_id, "Node removed from cluster");

        Ok(())
    }

    /// List active (non-deleted) containers running on a specific node.
    pub async fn list_containers_for_node(
        &self,
        node_id: i32,
    ) -> Result<Vec<deployment_containers::Model>, NodeError> {
        let containers = deployment_containers::Entity::find()
            .filter(deployment_containers::Column::NodeId.eq(node_id))
            .filter(deployment_containers::Column::DeletedAt.is_null())
            .all(self.db.as_ref())
            .await?;
        Ok(containers)
    }

    /// List active containers on a node for a specific deployment.
    pub async fn list_containers_for_node_deployment(
        &self,
        node_id: i32,
        deployment_id: i32,
    ) -> Result<Vec<deployment_containers::Model>, NodeError> {
        let containers = deployment_containers::Entity::find()
            .filter(deployment_containers::Column::NodeId.eq(node_id))
            .filter(deployment_containers::Column::DeploymentId.eq(deployment_id))
            .filter(deployment_containers::Column::DeletedAt.is_null())
            .all(self.db.as_ref())
            .await?;
        Ok(containers)
    }

    /// Get the set of unique (project_id, environment_id) pairs affected by
    /// containers running on the given node. Each pair represents an environment
    /// that has at least one live container on this node and may need redeployment.
    pub async fn affected_environments(&self, node_id: i32) -> Result<Vec<(i32, i32)>, NodeError> {
        let containers = self.list_containers_for_node(node_id).await?;

        // Gather unique deployment IDs
        let deployment_ids: Vec<i32> = containers
            .iter()
            .map(|c| c.deployment_id)
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();

        if deployment_ids.is_empty() {
            return Ok(vec![]);
        }

        let deploys = deployments::Entity::find()
            .filter(deployments::Column::Id.is_in(deployment_ids))
            .all(self.db.as_ref())
            .await?;

        let pairs: Vec<(i32, i32)> = deploys
            .iter()
            .map(|d| (d.project_id, d.environment_id))
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();

        Ok(pairs)
    }

    /// Detailed info about each deployment affected by a node drain.
    /// Returns `(project_id, environment_id, deployment_id, containers_on_node, total_active_containers)`.
    pub async fn affected_deployments(
        &self,
        node_id: i32,
    ) -> Result<Vec<AffectedDeployment>, NodeError> {
        let containers_on_node = self.list_containers_for_node(node_id).await?;

        let deployment_ids: Vec<i32> = containers_on_node
            .iter()
            .map(|c| c.deployment_id)
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();

        if deployment_ids.is_empty() {
            return Ok(vec![]);
        }

        let deploys = deployments::Entity::find()
            .filter(deployments::Column::Id.is_in(deployment_ids.clone()))
            .all(self.db.as_ref())
            .await?;

        // For each deployment, count ALL active containers (on any node)
        let all_containers = deployment_containers::Entity::find()
            .filter(deployment_containers::Column::DeploymentId.is_in(deployment_ids))
            .filter(deployment_containers::Column::DeletedAt.is_null())
            .all(self.db.as_ref())
            .await?;

        let mut result = Vec::new();
        for deploy in &deploys {
            let on_node = containers_on_node
                .iter()
                .filter(|c| c.deployment_id == deploy.id)
                .count();
            let total = all_containers
                .iter()
                .filter(|c| c.deployment_id == deploy.id)
                .count();

            result.push(AffectedDeployment {
                project_id: deploy.project_id,
                environment_id: deploy.environment_id,
                deployment_id: deploy.id,
                containers_on_node: on_node,
                total_active_containers: total,
            });
        }

        Ok(result)
    }

    /// Reconcile the control plane's container records against the agent's actual
    /// Docker state. Marks DB records as deleted if the container no longer exists
    /// on the worker node (ghost records from crashes / interrupted deploys).
    ///
    /// `actual_container_ids` is the list of temps-managed Docker container IDs
    /// currently running on the node (reported by the agent).
    ///
    /// Returns the number of stale DB records that were marked as deleted.
    pub async fn reconcile_containers(
        &self,
        node_id: i32,
        actual_container_ids: &[String],
    ) -> Result<usize, NodeError> {
        let db_containers = self.list_containers_for_node(node_id).await?;

        let mut stale_count = 0;
        for container in db_containers {
            if !actual_container_ids.contains(&container.container_id) {
                // Container exists in DB but not on the node — ghost record
                tracing::warn!(
                    node_id,
                    container_id = %container.container_id,
                    container_name = %container.container_name,
                    deployment_id = container.deployment_id,
                    "Reconciliation: container not found on node, marking as deleted"
                );

                let mut active: deployment_containers::ActiveModel = container.into();
                active.deleted_at = Set(Some(chrono::Utc::now()));
                active.status = Set(Some("removed".to_string()));
                active.update(self.db.as_ref()).await?;
                stale_count += 1;
            }
        }

        if stale_count > 0 {
            tracing::info!(
                node_id,
                stale_count,
                "Reconciliation complete: cleaned up {} ghost container record(s)",
                stale_count
            );
        }

        Ok(stale_count)
    }

    /// Soft-delete all active containers on a specific node for a given deployment.
    /// Sets `deleted_at` and `status = "removed"` so the proxy stops routing to them.
    /// Returns the number of containers retired.
    pub async fn retire_containers_on_node(
        &self,
        node_id: i32,
        deployment_id: i32,
    ) -> Result<usize, NodeError> {
        let containers = deployment_containers::Entity::find()
            .filter(deployment_containers::Column::NodeId.eq(node_id))
            .filter(deployment_containers::Column::DeploymentId.eq(deployment_id))
            .filter(deployment_containers::Column::DeletedAt.is_null())
            .all(self.db.as_ref())
            .await?;

        let count = containers.len();
        for container in containers {
            let mut active: deployment_containers::ActiveModel = container.into();
            active.deleted_at = Set(Some(chrono::Utc::now()));
            active.status = Set(Some("removed".to_string()));
            active.update(self.db.as_ref()).await?;
        }

        if count > 0 {
            tracing::info!(
                node_id,
                deployment_id,
                count,
                "Retired containers on draining node"
            );
        }

        Ok(count)
    }
}

/// Info about a deployment affected by a node going offline or draining.
#[derive(Debug, Clone)]
pub struct AffectedDeployment {
    pub project_id: i32,
    pub environment_id: i32,
    pub deployment_id: i32,
    /// Number of active containers for this deployment on the affected node.
    pub containers_on_node: usize,
    /// Total number of active containers for this deployment across all nodes.
    pub total_active_containers: usize,
}

impl AffectedDeployment {
    /// Returns true if removing containers on the affected node leaves zero replicas.
    /// In this case a full redeploy is needed to maintain availability.
    pub fn needs_redeploy(&self) -> bool {
        self.total_active_containers <= self.containers_on_node
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use temps_entities::deployments;

    fn sample_node() -> nodes::Model {
        nodes::Model {
            id: 1,
            name: "worker-1".to_string(),
            token_hash: "hash123".to_string(),
            token_encrypted: None,
            address: "https://10.100.0.2:3100".to_string(),
            private_address: "10.100.0.2".to_string(),
            public_endpoint: Some("203.0.113.50:51820".to_string()),
            wg_public_key: Some("pubkey123".to_string()),
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

    /// Build a register request with sensible defaults for tests.
    fn register_req(name: &str, token_hash: &str, address: &str) -> RegisterNodeRequest {
        RegisterNodeRequest {
            name: name.to_string(),
            token_hash: token_hash.to_string(),
            token_encrypted: None,
            address: address.to_string(),
            private_address: "10.100.0.2".to_string(),
            public_endpoint: None,
            wg_public_key: None,
            role: "worker".to_string(),
            labels: serde_json::json!({}),
            edge_public_key: None,
            prior_token_hash: None,
        }
    }

    #[tokio::test]
    async fn test_register_validates_empty_name() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let service = NodeService::new(Arc::new(db));

        let result = service
            .register(register_req("", "hash", "https://10.100.0.2:3100"))
            .await;

        assert!(matches!(result.unwrap_err(), NodeError::Validation { .. }));
    }

    #[tokio::test]
    async fn test_register_validates_empty_address() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let service = NodeService::new(Arc::new(db));

        let result = service.register(register_req("worker-1", "hash", "")).await;

        assert!(matches!(result.unwrap_err(), NodeError::Validation { .. }));
    }

    #[tokio::test]
    async fn test_register_reconnect_offline_node_rebinds() {
        // An OFFLINE node being re-provisioned with new credentials is the
        // legitimate re-registration case — it must succeed. (ADR-020 WS-1.2.)
        let mut offline = sample_node();
        offline.status = "offline".to_string();

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![offline]]) // find by name
            .append_query_results(vec![vec![sample_node()]]) // update
            .into_connection();
        let service = NodeService::new(Arc::new(db));

        let result = service
            .register(register_req(
                "worker-1",
                "new-hash",
                "https://10.100.0.3:3100",
            ))
            .await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap().name, "worker-1");
    }

    #[tokio::test]
    async fn test_register_idempotent_same_identity_reconnect() {
        // Re-registering with the SAME identity (no token/address/key change)
        // is not an identity change and is always allowed, even while live.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![sample_node()]]) // find by name (live)
            .append_query_results(vec![vec![sample_node()]]) // update
            .into_connection();
        let service = NodeService::new(Arc::new(db));

        let mut req = register_req("worker-1", "hash123", "https://10.100.0.2:3100");
        req.wg_public_key = Some("pubkey123".to_string()); // match sample_node

        let result = service.register(req).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_register_rejects_identity_change_on_live_node() {
        // A live node (active + recent heartbeat) cannot have its identity
        // silently rebound by a join-token holder with no proof. (enroll-1.)
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![sample_node()]]) // find by name (live)
            .into_connection();
        let service = NodeService::new(Arc::new(db));

        let result = service
            .register(register_req(
                "worker-1",
                "attacker-hash",
                "https://10.0.0.66:3100",
            ))
            .await;

        assert!(matches!(
            result.unwrap_err(),
            NodeError::IdentityConflict { .. }
        ));
    }

    #[tokio::test]
    async fn test_register_identity_change_with_proof_succeeds() {
        // The same identity change IS allowed when the caller proves possession
        // of the node's current token.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![sample_node()]]) // find by name (live)
            .append_query_results(vec![vec![sample_node()]]) // update
            .into_connection();
        let service = NodeService::new(Arc::new(db));

        let mut req = register_req("worker-1", "new-hash", "https://10.100.0.3:3100");
        req.prior_token_hash = Some("hash123".to_string()); // matches sample_node.token_hash

        let result = service.register(req).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_list_all() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![sample_node()]])
            .into_connection();
        let service = NodeService::new(Arc::new(db));

        let nodes = service.list_all().await.unwrap();
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].name, "worker-1");
    }

    #[tokio::test]
    async fn test_get_by_id_not_found() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<nodes::Model>::new()])
            .into_connection();
        let service = NodeService::new(Arc::new(db));

        let result = service.get_by_id(999).await;
        assert!(matches!(
            result.unwrap_err(),
            NodeError::NotFoundById { node_id: 999 }
        ));
    }

    fn sample_container(id: i32, deployment_id: i32, node_id: i32) -> deployment_containers::Model {
        deployment_containers::Model {
            id,
            deployment_id,
            container_id: format!("container-{}", id),
            container_name: format!("app-{}", id),
            container_port: 8080,
            host_port: Some(30000 + id),
            image_name: Some("myapp:latest".to_string()),
            status: Some("running".to_string()),
            service_name: None,
            created_at: chrono::Utc::now(),
            deployed_at: chrono::Utc::now(),
            ready_at: Some(chrono::Utc::now()),
            deleted_at: None,
            node_id: Some(node_id),
            exit_code: None,
            exit_reason: None,
            oom_killed: None,
            error_message: None,
            finished_at: None,
            started_at: None,
            cpu_limit_cores: None,
        }
    }

    fn sample_deployment(id: i32, project_id: i32, environment_id: i32) -> deployments::Model {
        deployments::Model {
            id,
            project_id,
            environment_id,
            slug: format!("deploy-{}", id),
            state: "deployed".to_string(),
            metadata: None,
            deploying_at: None,
            ready_at: None,
            started_at: Some(chrono::Utc::now()),
            finished_at: Some(chrono::Utc::now()),
            context_vars: None,
            branch_ref: Some("main".to_string()),
            tag_ref: None,
            commit_sha: None,
            commit_message: None,
            commit_author: None,
            commit_json: None,
            cancelled_reason: None,
            static_dir_location: None,
            screenshot_location: None,
            image_name: None,
            deployment_config: None,
            promoted_from_deployment_id: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    #[tokio::test]
    async fn test_list_containers_for_node_returns_active_containers() {
        let c1 = sample_container(1, 10, 5);
        let c2 = sample_container(2, 11, 5);

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![c1.clone(), c2.clone()]])
            .into_connection();
        let service = NodeService::new(Arc::new(db));

        let containers = service.list_containers_for_node(5).await.unwrap();
        assert_eq!(containers.len(), 2);
        assert_eq!(containers[0].container_id, "container-1");
        assert_eq!(containers[1].container_id, "container-2");
    }

    #[tokio::test]
    async fn test_list_containers_for_node_empty() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<deployment_containers::Model>::new()])
            .into_connection();
        let service = NodeService::new(Arc::new(db));

        let containers = service.list_containers_for_node(99).await.unwrap();
        assert!(containers.is_empty());
    }

    #[tokio::test]
    async fn test_affected_environments_returns_unique_pairs() {
        // Two containers on the same deployment, one on a different deployment
        let c1 = sample_container(1, 10, 5);
        let c2 = sample_container(2, 10, 5); // same deployment
        let c3 = sample_container(3, 20, 5); // different deployment

        let d1 = sample_deployment(10, 100, 200);
        let d2 = sample_deployment(20, 100, 201);

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // list_containers_for_node
            .append_query_results(vec![vec![c1, c2, c3]])
            // deployments query
            .append_query_results(vec![vec![d1, d2]])
            .into_connection();
        let service = NodeService::new(Arc::new(db));

        let affected = service.affected_environments(5).await.unwrap();
        // Should have 2 unique (project_id, environment_id) pairs
        assert_eq!(affected.len(), 2);
        assert!(affected.contains(&(100, 200)));
        assert!(affected.contains(&(100, 201)));
    }

    #[tokio::test]
    async fn test_affected_environments_empty_when_no_containers() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<deployment_containers::Model>::new()])
            .into_connection();
        let service = NodeService::new(Arc::new(db));

        let affected = service.affected_environments(99).await.unwrap();
        assert!(affected.is_empty());
    }

    #[tokio::test]
    async fn test_check_drain_complete_transitions_to_drained() {
        // Node is "draining" with 0 containers → should transition to "drained"
        let mut draining_node = sample_node();
        draining_node.status = "draining".to_string();

        let mut drained_node = sample_node();
        drained_node.status = "drained".to_string();

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // check_drain_complete: find_by_id
            .append_query_results(vec![vec![draining_node.clone()]])
            // list_containers_for_node: empty → drain complete
            .append_query_results(vec![Vec::<deployment_containers::Model>::new()])
            // update status to "drained"
            .append_query_results(vec![vec![drained_node]])
            .into_connection();
        let service = NodeService::new(Arc::new(db));

        let result = service.check_drain_complete(1).await.unwrap();
        assert!(result, "Should return true when drain completes");
    }

    #[tokio::test]
    async fn test_check_drain_complete_still_has_containers() {
        // Node is "draining" with containers still running → should not transition
        let mut draining_node = sample_node();
        draining_node.status = "draining".to_string();

        let container = sample_container(1, 10, 1);

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // check_drain_complete: find_by_id
            .append_query_results(vec![vec![draining_node]])
            // list_containers_for_node: has containers
            .append_query_results(vec![vec![container]])
            .into_connection();
        let service = NodeService::new(Arc::new(db));

        let result = service.check_drain_complete(1).await.unwrap();
        assert!(!result, "Should return false when containers remain");
    }

    #[tokio::test]
    async fn test_check_drain_complete_already_drained() {
        // Node is already "drained" → should return true immediately
        let mut drained_node = sample_node();
        drained_node.status = "drained".to_string();

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![drained_node]])
            .into_connection();
        let service = NodeService::new(Arc::new(db));

        let result = service.check_drain_complete(1).await.unwrap();
        assert!(result, "Should return true for already-drained node");
    }

    #[tokio::test]
    async fn test_check_drain_complete_active_node() {
        // Node is "active" (not draining) → should return false
        let active_node = sample_node(); // status = "active"

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![active_node]])
            .into_connection();
        let service = NodeService::new(Arc::new(db));

        let result = service.check_drain_complete(1).await.unwrap();
        assert!(!result, "Should return false for active node");
    }

    #[tokio::test]
    async fn test_check_drain_complete_not_found() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<nodes::Model>::new()])
            .into_connection();
        let service = NodeService::new(Arc::new(db));

        let result = service.check_drain_complete(999).await;
        assert!(matches!(
            result.unwrap_err(),
            NodeError::NotFoundById { node_id: 999 }
        ));
    }

    #[tokio::test]
    async fn test_check_all_drains() {
        let mut draining_node = sample_node();
        draining_node.status = "draining".to_string();

        let mut drained_result = sample_node();
        drained_result.status = "drained".to_string();

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // check_all_drains: find draining nodes
            .append_query_results(vec![vec![draining_node.clone()]])
            // check_drain_complete: find_by_id for node 1
            .append_query_results(vec![vec![draining_node]])
            // list_containers_for_node: empty
            .append_query_results(vec![Vec::<deployment_containers::Model>::new()])
            // update: transitions to drained
            .append_query_results(vec![vec![drained_result]])
            .into_connection();
        let service = NodeService::new(Arc::new(db));

        let completed = service.check_all_drains().await.unwrap();
        assert_eq!(completed, vec![1]);
    }

    // ── Heartbeat preserves managed status ───────────────────────

    #[tokio::test]
    async fn test_heartbeat_preserves_draining_status() {
        let mut draining_node = sample_node();
        draining_node.status = "draining".to_string();

        // The updated node should still be "draining" (status unchanged)
        let mut updated_node = sample_node();
        updated_node.status = "draining".to_string();

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // find_by_id
            .append_query_results(vec![vec![draining_node]])
            // update
            .append_query_results(vec![vec![updated_node]])
            .into_connection();
        let service = NodeService::new(Arc::new(db));

        let result = service
            .heartbeat(
                1,
                HeartbeatRequest {
                    capacity: serde_json::json!({"cpu": 50}),
                    labels: None,
                },
            )
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_heartbeat_reactivates_offline_node() {
        let mut offline_node = sample_node();
        offline_node.status = "offline".to_string();

        let mut reactivated_node = sample_node();
        reactivated_node.status = "active".to_string();

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![offline_node]])
            .append_query_results(vec![vec![reactivated_node]])
            .into_connection();
        let service = NodeService::new(Arc::new(db));

        let result = service
            .heartbeat(
                1,
                HeartbeatRequest {
                    capacity: serde_json::json!({"cpu": 50}),
                    labels: None,
                },
            )
            .await;
        assert!(result.is_ok());
    }

    // ── AffectedDeployment unit tests ──────────────────────────────

    #[test]
    fn test_needs_redeploy_all_on_draining_node() {
        let dep = AffectedDeployment {
            project_id: 1,
            environment_id: 2,
            deployment_id: 10,
            containers_on_node: 3,
            total_active_containers: 3,
        };
        assert!(dep.needs_redeploy());
    }

    #[test]
    fn test_needs_redeploy_some_on_other_nodes() {
        let dep = AffectedDeployment {
            project_id: 1,
            environment_id: 2,
            deployment_id: 10,
            containers_on_node: 1,
            total_active_containers: 4,
        };
        assert!(!dep.needs_redeploy());
    }

    #[test]
    fn test_needs_redeploy_single_replica_on_node() {
        let dep = AffectedDeployment {
            project_id: 1,
            environment_id: 2,
            deployment_id: 10,
            containers_on_node: 1,
            total_active_containers: 1,
        };
        assert!(dep.needs_redeploy());
    }

    // ── affected_deployments integration tests ─────────────────────

    #[tokio::test]
    async fn test_affected_deployments_mixed_replicas() {
        // Deployment 10: 2 containers on node 5, 4 total (has healthy replicas elsewhere)
        // Deployment 20: 1 container on node 5, 1 total (needs redeploy)
        let c1 = sample_container(1, 10, 5);
        let c2 = sample_container(2, 10, 5);
        let c3 = sample_container(3, 20, 5);

        let d1 = sample_deployment(10, 100, 200);
        let d2 = sample_deployment(20, 100, 201);

        // "All active containers" includes ones on other nodes
        let c_other_node = sample_container(4, 10, 7); // deployment 10 on node 7
        let c_other_node2 = sample_container(5, 10, 8); // deployment 10 on node 8

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // list_containers_for_node(5): containers on the draining node
            .append_query_results(vec![vec![c1.clone(), c2.clone(), c3.clone()]])
            // deployments query
            .append_query_results(vec![vec![d1, d2]])
            // all active containers for these deployment IDs
            .append_query_results(vec![vec![c1, c2, c_other_node, c_other_node2, c3]])
            .into_connection();
        let service = NodeService::new(Arc::new(db));

        let affected = service.affected_deployments(5).await.unwrap();
        assert_eq!(affected.len(), 2);

        let dep10 = affected.iter().find(|d| d.deployment_id == 10).unwrap();
        assert_eq!(dep10.containers_on_node, 2);
        assert_eq!(dep10.total_active_containers, 4);
        assert!(!dep10.needs_redeploy()); // 2 remain on other nodes

        let dep20 = affected.iter().find(|d| d.deployment_id == 20).unwrap();
        assert_eq!(dep20.containers_on_node, 1);
        assert_eq!(dep20.total_active_containers, 1);
        assert!(dep20.needs_redeploy()); // all replicas on this node
    }

    #[tokio::test]
    async fn test_affected_deployments_empty() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<deployment_containers::Model>::new()])
            .into_connection();
        let service = NodeService::new(Arc::new(db));

        let affected = service.affected_deployments(99).await.unwrap();
        assert!(affected.is_empty());
    }

    #[tokio::test]
    async fn test_retire_containers_on_node() {
        // 2 containers for deployment 10 on node 5
        let c1 = sample_container(1, 10, 5);
        let c2 = sample_container(2, 10, 5);

        // After update, containers come back with deleted status
        let mut c1_updated = sample_container(1, 10, 5);
        c1_updated.status = Some("removed".to_string());
        c1_updated.deleted_at = Some(chrono::Utc::now());
        let mut c2_updated = sample_container(2, 10, 5);
        c2_updated.status = Some("removed".to_string());
        c2_updated.deleted_at = Some(chrono::Utc::now());

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // find containers on node for deployment
            .append_query_results(vec![vec![c1, c2]])
            // update c1
            .append_query_results(vec![vec![c1_updated]])
            // update c2
            .append_query_results(vec![vec![c2_updated]])
            .into_connection();
        let service = NodeService::new(Arc::new(db));

        let count = service.retire_containers_on_node(5, 10).await.unwrap();
        assert_eq!(count, 2);
    }

    // ── mark_active (undrain) tests ──────────────────────────────

    #[tokio::test]
    async fn test_mark_active_from_draining() {
        let mut draining_node = sample_node();
        draining_node.status = "draining".to_string();

        let mut reactivated = sample_node();
        reactivated.status = "active".to_string();

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![draining_node]])
            .append_query_results(vec![vec![reactivated]])
            .into_connection();
        let service = NodeService::new(Arc::new(db));

        let result = service.mark_active(1).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_mark_active_from_drained() {
        let mut drained_node = sample_node();
        drained_node.status = "drained".to_string();

        let mut reactivated = sample_node();
        reactivated.status = "active".to_string();

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![drained_node]])
            .append_query_results(vec![vec![reactivated]])
            .into_connection();
        let service = NodeService::new(Arc::new(db));

        let result = service.mark_active(1).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_mark_active_rejects_active_node() {
        let active_node = sample_node(); // status = "active"

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![active_node]])
            .into_connection();
        let service = NodeService::new(Arc::new(db));

        let result = service.mark_active(1).await;
        assert!(matches!(result.unwrap_err(), NodeError::Validation { .. }));
    }

    #[tokio::test]
    async fn test_mark_active_rejects_offline_node() {
        let mut offline_node = sample_node();
        offline_node.status = "offline".to_string();

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![offline_node]])
            .into_connection();
        let service = NodeService::new(Arc::new(db));

        let result = service.mark_active(1).await;
        assert!(matches!(result.unwrap_err(), NodeError::Validation { .. }));
    }

    #[tokio::test]
    async fn test_mark_active_not_found() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<nodes::Model>::new()])
            .into_connection();
        let service = NodeService::new(Arc::new(db));

        let result = service.mark_active(999).await;
        assert!(matches!(
            result.unwrap_err(),
            NodeError::NotFoundById { node_id: 999 }
        ));
    }

    #[tokio::test]
    async fn test_retire_containers_on_node_empty() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<deployment_containers::Model>::new()])
            .into_connection();
        let service = NodeService::new(Arc::new(db));

        let count = service.retire_containers_on_node(5, 10).await.unwrap();
        assert_eq!(count, 0);
    }

    // ── reconcile_containers tests ──────────────────────────────

    #[tokio::test]
    async fn test_reconcile_marks_ghost_containers_as_deleted() {
        // DB has two containers, but agent only reports one
        let c1 = sample_container(1, 10, 5); // container_id = "container-1"
        let c2 = sample_container(2, 11, 5); // container_id = "container-2"

        let mut c2_updated = sample_container(2, 11, 5);
        c2_updated.status = Some("removed".to_string());
        c2_updated.deleted_at = Some(chrono::Utc::now());

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // list_containers_for_node
            .append_query_results(vec![vec![c1, c2]])
            // update c2 (ghost)
            .append_query_results(vec![vec![c2_updated]])
            .into_connection();
        let service = NodeService::new(Arc::new(db));

        // Agent reports only container-1 is running
        let actual = vec!["container-1".to_string()];
        let stale = service.reconcile_containers(5, &actual).await.unwrap();
        assert_eq!(stale, 1);
    }

    #[tokio::test]
    async fn test_reconcile_no_stale_containers() {
        // DB has two containers, agent reports both
        let c1 = sample_container(1, 10, 5);
        let c2 = sample_container(2, 11, 5);

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![c1, c2]])
            .into_connection();
        let service = NodeService::new(Arc::new(db));

        let actual = vec!["container-1".to_string(), "container-2".to_string()];
        let stale = service.reconcile_containers(5, &actual).await.unwrap();
        assert_eq!(stale, 0);
    }

    #[tokio::test]
    async fn test_reconcile_empty_node() {
        // DB has no containers, agent reports none
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<deployment_containers::Model>::new()])
            .into_connection();
        let service = NodeService::new(Arc::new(db));

        let stale = service.reconcile_containers(5, &[]).await.unwrap();
        assert_eq!(stale, 0);
    }

    #[tokio::test]
    async fn test_reconcile_all_containers_gone() {
        // DB has two containers, agent reports none (all crashed)
        let c1 = sample_container(1, 10, 5);
        let c2 = sample_container(2, 11, 5);

        let mut c1_updated = sample_container(1, 10, 5);
        c1_updated.status = Some("removed".to_string());
        c1_updated.deleted_at = Some(chrono::Utc::now());
        let mut c2_updated = sample_container(2, 11, 5);
        c2_updated.status = Some("removed".to_string());
        c2_updated.deleted_at = Some(chrono::Utc::now());

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // list_containers_for_node
            .append_query_results(vec![vec![c1, c2]])
            // update c1
            .append_query_results(vec![vec![c1_updated]])
            // update c2
            .append_query_results(vec![vec![c2_updated]])
            .into_connection();
        let service = NodeService::new(Arc::new(db));

        let stale = service.reconcile_containers(5, &[]).await.unwrap();
        assert_eq!(stale, 2);
    }
}
