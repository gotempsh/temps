// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Read-only lookups behind the node API: edge nodes, the S3 source a node asks
//! for, and what runs on a node.

use super::*;
use std::collections::{HashMap, HashSet};
use temps_entities::s3_sources;

/// A container running on a node, with its project and environment named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeContainerRecord {
    pub container_id: String,
    pub container_name: String,
    pub image_name: String,
    pub status: String,
    pub created_at: String,
    pub deployment_id: i32,
    pub project_id: i32,
    pub project_name: String,
    pub environment_id: i32,
    pub environment_name: String,
}

impl NodeService {
    /// The S3 source a node asked for. The caller has already authorized the
    /// node for it with [`Self::is_authorized_for_s3_source`].
    pub async fn s3_source(&self, s3_source_id: i32) -> Result<s3_sources::Model, NodeError> {
        s3_sources::Entity::find_by_id(s3_source_id)
            .one(self.db.as_ref())
            .await?
            .ok_or(NodeError::S3SourceNotFound { s3_source_id })
    }

    /// Every edge node, whatever its status.
    pub async fn edge_nodes(&self) -> Result<Vec<nodes::Model>, NodeError> {
        Ok(nodes::Entity::find()
            .filter(nodes::Column::Role.eq("edge"))
            .all(self.db.as_ref())
            .await?)
    }

    /// The active edge nodes to query, narrowed to one when `node_id` is given.
    pub async fn active_edge_nodes(
        &self,
        node_id: Option<i32>,
    ) -> Result<Vec<nodes::Model>, NodeError> {
        let mut finder = nodes::Entity::find().filter(nodes::Column::Role.eq("edge"));
        if let Some(nid) = node_id {
            finder = finder.filter(nodes::Column::Id.eq(nid));
        }
        // Only query active nodes
        finder = finder.filter(nodes::Column::Status.eq("active"));
        Ok(finder.all(self.db.as_ref()).await?)
    }

    /// The live containers on a node. `None` is the control plane, whose own
    /// containers (the `Local` scheduling slot) are stored with node_id = NULL.
    ///
    /// A failed name lookup falls back to `project-{id}` / `env-{id}`: the list
    /// is still useful without the names.
    pub async fn containers_on_node(
        &self,
        node_id: Option<i32>,
    ) -> Result<Vec<NodeContainerRecord>, NodeError> {
        let node_filter = match node_id {
            None => deployment_containers::Column::NodeId.is_null(),
            Some(node_id) => deployment_containers::Column::NodeId.eq(node_id),
        };

        // Query containers for this node, joining with deployments, projects, and environments
        let rows: Vec<(deployment_containers::Model, Option<deployments::Model>)> =
            deployment_containers::Entity::find()
                .filter(node_filter)
                .filter(deployment_containers::Column::DeletedAt.is_null())
                .find_also_related(deployments::Entity)
                .all(self.db.as_ref())
                .await?;

        // Collect unique project and environment IDs
        let mut project_ids = HashSet::new();
        let mut environment_ids = HashSet::new();
        for (_, deployment) in &rows {
            if let Some(d) = deployment {
                project_ids.insert(d.project_id);
                environment_ids.insert(d.environment_id);
            }
        }

        // Batch-fetch project names
        let projects: HashMap<i32, String> = projects::Entity::find()
            .filter(projects::Column::Id.is_in(project_ids))
            .all(self.db.as_ref())
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|p| (p.id, p.name))
            .collect();

        // Batch-fetch environment names
        let environments: HashMap<i32, String> = environments::Entity::find()
            .filter(environments::Column::Id.is_in(environment_ids))
            .all(self.db.as_ref())
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|e| (e.id, e.name))
            .collect();

        Ok(rows
            .into_iter()
            .filter_map(|(container, deployment)| {
                let d = deployment?;
                Some(NodeContainerRecord {
                    container_id: container.container_id,
                    container_name: container.container_name,
                    image_name: container.image_name.unwrap_or_default(),
                    status: container.status.unwrap_or_else(|| "unknown".to_string()),
                    created_at: container.created_at.to_rfc3339(),
                    deployment_id: d.id,
                    project_id: d.project_id,
                    project_name: projects
                        .get(&d.project_id)
                        .cloned()
                        .unwrap_or_else(|| format!("project-{}", d.project_id)),
                    environment_id: d.environment_id,
                    environment_name: environments
                        .get(&d.environment_id)
                        .cloned()
                        .unwrap_or_else(|| format!("env-{}", d.environment_id)),
                })
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, DbErr, MockDatabase};

    const NOW: &str = "2026-01-01T00:00:00Z";

    fn service(db: MockDatabase) -> NodeService {
        NodeService::new(Arc::new(db.into_connection()))
    }

    fn mock() -> MockDatabase {
        MockDatabase::new(DatabaseBackend::Postgres)
    }

    fn lost() -> DbErr {
        DbErr::Custom("connection lost".to_string())
    }

    fn logged(db: Arc<DatabaseConnection>) -> String {
        let db = Arc::try_unwrap(db).unwrap_or_else(|_| panic!("db still has owners"));
        format!("{:?}", db.into_transaction_log())
    }

    fn container(id: &str) -> deployment_containers::Model {
        serde_json::from_value(serde_json::json!({
            "id": 1,
            "deployment_id": 8,
            "container_id": id,
            "container_name": format!("{id}-name"),
            "health_state": "healthy",
            "created_at": NOW,
            "deployed_at": NOW
        }))
        .expect("container fixture matches the entity")
    }

    fn deployment() -> deployments::Model {
        serde_json::from_value(serde_json::json!({
            "id": 8,
            "project_id": 3,
            "environment_id": 4,
            "slug": "deployment-8",
            "state": "running",
            "created_at": NOW,
            "updated_at": NOW,
            "docker_socket_mounted": false
        }))
        .expect("deployment fixture matches the entity")
    }

    #[tokio::test]
    async fn a_missing_s3_source_is_reported_with_its_id() {
        let error = service(mock().append_query_results([Vec::<s3_sources::Model>::new()]))
            .s3_source(12)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            NodeError::S3SourceNotFound { s3_source_id: 12 }
        ));
        assert!(error.to_string().contains("12"));
    }

    #[tokio::test]
    async fn a_failed_s3_source_lookup_is_a_database_error() {
        let error = service(mock().append_query_errors([lost()]))
            .s3_source(12)
            .await
            .unwrap_err();
        assert!(matches!(error, NodeError::Database(_)));
    }

    #[tokio::test]
    async fn listing_edge_nodes_does_not_filter_by_status() {
        let db = Arc::new(
            mock()
                .append_query_results([Vec::<nodes::Model>::new()])
                .into_connection(),
        );
        let node_service = NodeService::new(db.clone());

        assert!(node_service.edge_nodes().await.unwrap().is_empty());

        drop(node_service);
        let log = logged(db);
        assert!(log.contains("role"), "{log}");
        assert!(!log.contains("\\\"status\\\" ="), "{log}");
    }

    #[tokio::test]
    async fn querying_edge_nodes_keeps_to_active_ones_and_the_requested_node() {
        let db = Arc::new(
            mock()
                .append_query_results([Vec::<nodes::Model>::new()])
                .into_connection(),
        );
        let node_service = NodeService::new(db.clone());

        node_service.active_edge_nodes(Some(9)).await.unwrap();

        drop(node_service);
        let log = logged(db);
        assert!(log.contains("active"), "{log}");
        assert!(log.contains("Int(Some(9))"), "{log}");
    }

    #[tokio::test]
    async fn failed_edge_node_queries_are_database_errors() {
        assert!(matches!(
            service(mock().append_query_errors([lost()]))
                .edge_nodes()
                .await,
            Err(NodeError::Database(_))
        ));
        assert!(matches!(
            service(mock().append_query_errors([lost()]))
                .active_edge_nodes(None)
                .await,
            Err(NodeError::Database(_))
        ));
    }

    /// The control plane's own containers are the ones with no node.
    #[tokio::test]
    async fn the_control_plane_is_the_containers_without_a_node() {
        let db = Arc::new(
            mock()
                .append_query_results([Vec::<(
                    deployment_containers::Model,
                    Option<deployments::Model>,
                )>::new()])
                .append_query_results([Vec::<projects::Model>::new()])
                .append_query_results([Vec::<environments::Model>::new()])
                .into_connection(),
        );
        let node_service = NodeService::new(db.clone());

        assert!(node_service
            .containers_on_node(None)
            .await
            .unwrap()
            .is_empty());

        drop(node_service);
        let log = logged(db);
        assert!(log.contains("node_id\\\" IS NULL"), "{log}");
        assert!(log.contains("deleted_at\\\" IS NULL"), "{log}");
    }

    /// A row with no deployment is dropped; a failed name lookup falls back to
    /// the ID rather than failing the list.
    #[tokio::test]
    async fn containers_are_named_with_a_fallback_and_orphans_are_dropped() {
        let node_service = service(
            mock()
                .append_query_results([vec![
                    (container("app"), Some(deployment())),
                    (container("orphan"), None),
                ]])
                .append_query_errors([lost()])
                .append_query_errors([lost()]),
        );

        let records = node_service.containers_on_node(Some(5)).await.unwrap();

        assert_eq!(records.len(), 1);
        let record = &records[0];
        assert_eq!(record.container_id, "app");
        assert_eq!(record.deployment_id, 8);
        assert_eq!(record.project_name, "project-3");
        assert_eq!(record.environment_name, "env-4");
        assert_eq!(record.image_name, "");
        assert_eq!(record.status, "unknown");
    }

    #[tokio::test]
    async fn a_failed_container_query_is_a_database_error() {
        assert!(matches!(
            service(mock().append_query_errors([lost()]))
                .containers_on_node(Some(5))
                .await,
            Err(NodeError::Database(_))
        ));
    }
}
