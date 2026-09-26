// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::{collections::HashSet, sync::Arc};

use sea_orm::{
    sea_query::OnConflict, ColumnTrait, ConnectionTrait, EntityTrait, PaginatorTrait, QueryFilter,
    Set, TransactionTrait,
};
use serde::{Deserialize, Serialize};
use temps_entities::{build_node_policies as policies, nodes, projects};
use utoipa::ToSchema;

#[derive(Debug, thiserror::Error)]
pub enum BuildNodePolicyError {
    #[error("Project {project_id} does not exist or has been deleted")]
    ProjectNotFound { project_id: i32 },
    #[error("Invalid builder-node policy: {message}")]
    Validation { message: String },
    #[error("Failed to {operation} builder-node policy for {scope}: {source}")]
    Database {
        operation: &'static str,
        scope: String,
        source: sea_orm::DbErr,
    },
}

/// PUT replaces the complete selection. null clears it (project: inherit;
/// global: automatic). An empty list is invalid, never an implicit fallback.
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SetBuildNodesRequest {
    /// Ordered worker IDs (1–100, unique). null restores inheritance/automatic
    /// selection. This does not change node roles or application placement.
    #[schema(required = true)]
    #[serde(deserialize_with = "required_node_ids")]
    pub node_ids: Option<Vec<i32>>,
}

fn required_node_ids<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Vec<i32>>, D::Error> {
    Option::<Vec<i32>>::deserialize(deserializer)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum BuildNodePolicySource {
    Automatic,
    Global,
    Project,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct BuildNodePolicyResponse {
    pub project_id: Option<i32>,
    /// Selection configured at this scope; null means inherit/automatic.
    pub node_ids: Option<Vec<i32>>,
    /// Exclusive, ordered builder candidates after resolving inheritance.
    pub effective_node_ids: Option<Vec<i32>>,
    pub source: BuildNodePolicySource,
}

impl BuildNodePolicyResponse {
    pub fn builds_locally(&self, local_workloads_enabled: bool) -> bool {
        local_workloads_enabled && self.effective_node_ids.is_none()
    }
}

pub struct BuildNodePolicyService {
    db: Arc<sea_orm::DatabaseConnection>,
}

fn scope(project_id: Option<i32>) -> String {
    project_id.map_or_else(|| "global".into(), |id| format!("project:{id}"))
}

pub fn validate_node_ids(ids: &[i32]) -> Result<(), BuildNodePolicyError> {
    if ids.is_empty()
        || ids.len() > 100
        || ids.iter().any(|id| *id <= 0)
        || ids.iter().collect::<HashSet<_>>().len() != ids.len()
    {
        return Err(BuildNodePolicyError::Validation {
            message: "node_ids must contain 1–100 distinct positive worker IDs; use null to clear the selection. The virtual control-plane node (0) cannot be selected.".into(),
        });
    }
    Ok(())
}

impl BuildNodePolicyService {
    pub fn new(db: Arc<sea_orm::DatabaseConnection>) -> Self {
        Self { db }
    }

    async fn ensure_project(
        db: &impl ConnectionTrait,
        project_id: Option<i32>,
    ) -> Result<(), BuildNodePolicyError> {
        if let Some(project_id) = project_id {
            let count = projects::Entity::find_by_id(project_id)
                .filter(projects::Column::IsDeleted.eq(false))
                .count(db)
                .await
                .map_err(|source| BuildNodePolicyError::Database {
                    operation: "check project for",
                    scope: scope(Some(project_id)),
                    source,
                })?;
            if count == 0 {
                return Err(BuildNodePolicyError::ProjectNotFound { project_id });
            }
        }
        Ok(())
    }

    pub async fn get(
        &self,
        project_id: Option<i32>,
    ) -> Result<BuildNodePolicyResponse, BuildNodePolicyError> {
        Self::ensure_project(self.db.as_ref(), project_id).await?;
        self.resolve(project_id).await
    }

    /// One bounded query for the global row and this project's row. Database
    /// errors propagate: a failed policy lookup must not move source elsewhere.
    pub async fn resolve(
        &self,
        project_id: Option<i32>,
    ) -> Result<BuildNodePolicyResponse, BuildNodePolicyError> {
        Self::resolve_on(self.db.as_ref(), project_id).await
    }

    async fn resolve_on(
        db: &impl ConnectionTrait,
        project_id: Option<i32>,
    ) -> Result<BuildNodePolicyResponse, BuildNodePolicyError> {
        let key = scope(project_id);
        let rows = policies::Entity::find()
            .filter(policies::Column::Scope.is_in(["global".to_string(), key.clone()]))
            .all(db)
            .await
            .map_err(|source| BuildNodePolicyError::Database {
                operation: "read",
                scope: key.clone(),
                source,
            })?;
        for row in &rows {
            validate_node_ids(&row.node_ids)?;
        }
        Ok(resolve_rows(project_id, &rows))
    }

    pub async fn set(
        &self,
        project_id: Option<i32>,
        ids: Option<Vec<i32>>,
    ) -> Result<BuildNodePolicyResponse, BuildNodePolicyError> {
        let key = scope(project_id);
        if let Some(ids) = ids.as_deref() {
            validate_node_ids(ids)?;
        }
        let txn = self
            .db
            .begin()
            .await
            .map_err(|source| BuildNodePolicyError::Database {
                operation: "begin update of",
                scope: key.clone(),
                source,
            })?;
        Self::ensure_project(&txn, project_id).await?;
        if let Some(ids) = ids {
            // Validate all IDs in one query. Allow temporarily offline nodes,
            // but never permit an edge-only node to receive application source.
            let found = nodes::Entity::find()
                .filter(nodes::Column::Id.is_in(ids.clone()))
                .all(&txn)
                .await
                .map_err(|source| BuildNodePolicyError::Database {
                    operation: "validate nodes for",
                    scope: key.clone(),
                    source,
                })?;
            let invalid: Vec<_> = ids
                .iter()
                .filter(|id| {
                    !found
                        .iter()
                        .any(|node| node.id == **id && node.role == "worker")
                })
                .copied()
                .collect();
            if !invalid.is_empty() {
                return Err(BuildNodePolicyError::Validation {
                    message: format!("Node IDs {invalid:?} are missing or are not worker nodes"),
                });
            }
            policies::Entity::insert(policies::ActiveModel {
                scope: Set(key.clone()),
                project_id: Set(project_id),
                node_ids: Set(ids),
            })
            .on_conflict(
                OnConflict::column(policies::Column::Scope)
                    .update_column(policies::Column::NodeIds)
                    .to_owned(),
            )
            .exec(&txn)
            .await
            .map_err(|source| BuildNodePolicyError::Database {
                operation: "save",
                scope: key.clone(),
                source,
            })?;
        } else {
            policies::Entity::delete_by_id(key.clone())
                .exec(&txn)
                .await
                .map_err(|source| BuildNodePolicyError::Database {
                    operation: "clear",
                    scope: key.clone(),
                    source,
                })?;
        }
        // A failed readback must not leave a committed, unaudited routing change.
        // Dropping the transaction on any error rolls the mutation back.
        let response = Self::resolve_on(&txn, project_id).await?;
        txn.commit()
            .await
            .map_err(|source| BuildNodePolicyError::Database {
                operation: "commit update of",
                scope: key,
                source,
            })?;
        Ok(response)
    }
}

fn resolve_rows(project_id: Option<i32>, rows: &[policies::Model]) -> BuildNodePolicyResponse {
    let own = rows
        .iter()
        .find(|row| row.scope == scope(project_id))
        .map(|row| row.node_ids.clone());
    let global = rows
        .iter()
        .find(|row| row.scope == "global")
        .map(|row| row.node_ids.clone());
    let effective = own.clone().or(global);
    let source = if own.is_some() && project_id.is_some() {
        BuildNodePolicySource::Project
    } else if effective.is_some() {
        BuildNodePolicySource::Global
    } else {
        BuildNodePolicySource::Automatic
    };
    BuildNodePolicyResponse {
        project_id,
        node_ids: own,
        effective_node_ids: effective,
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};

    #[tokio::test]
    async fn reads_global_policy_and_propagates_storage_failure() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![policies::Model {
                scope: "global".into(),
                project_id: None,
                node_ids: vec![4],
            }]])
            .append_query_errors(vec![sea_orm::DbErr::Custom("unavailable".into())])
            .into_connection();
        let service = BuildNodePolicyService::new(Arc::new(db));
        assert_eq!(
            service.get(None).await.unwrap().effective_node_ids,
            Some(vec![4])
        );
        assert!(matches!(
            service.get(None).await,
            Err(BuildNodePolicyError::Database { .. })
        ));
    }

    #[tokio::test]
    async fn clearing_global_restores_automatic_and_persists_delete() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .append_query_results(vec![Vec::<policies::Model>::new()])
            .into_connection();
        let service = BuildNodePolicyService::new(Arc::new(db));
        assert_eq!(
            service.set(None, None).await.unwrap().source,
            BuildNodePolicySource::Automatic
        );
    }

    #[tokio::test]
    async fn readback_failure_rolls_back_policy_change() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_exec_results(vec![MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 1,
                }])
                .append_query_errors(vec![sea_orm::DbErr::Custom("readback unavailable".into())])
                .into_connection(),
        );
        let service = BuildNodePolicyService::new(db.clone());
        assert!(matches!(
            service.set(None, None).await,
            Err(BuildNodePolicyError::Database { .. })
        ));
        drop(service);
        let db = Arc::try_unwrap(db).unwrap_or_else(|_| panic!("database still shared"));
        let log = format!("{:?}", db.into_transaction_log());
        assert!(log.contains("BEGIN"), "{log}");
        assert!(log.contains("DELETE"), "{log}");
        assert!(log.contains("ROLLBACK"), "{log}");
        assert!(!log.contains("COMMIT"), "{log}");
    }

    #[tokio::test]
    async fn missing_nodes_are_rejected_before_save() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<nodes::Model>::new()])
            .into_connection();
        let service = BuildNodePolicyService::new(Arc::new(db));
        assert!(matches!(
            service.set(None, Some(vec![99])).await,
            Err(BuildNodePolicyError::Validation { .. })
        ));
    }

    #[tokio::test]
    async fn upsert_keeps_order_and_returns_saved_policy() {
        let now = chrono::Utc::now();
        let node: nodes::Model = serde_json::from_value(serde_json::json!({
            "id": 9, "name": "builder", "token_hash": "test", "address": "http://example.test:3100",
            "private_address": "192.0.2.1", "role": "worker", "status": "offline", "labels": {},
            "capacity": {}, "dns_resolver_consecutive_failures": 0, "public_ingress_enabled": false, "public_ingress_running": false,
            "public_ingress_unsupported_reasons": [], "created_at": now, "updated_at": now
        }))
        .unwrap();
        let row = policies::Model {
            scope: "global".into(),
            project_id: None,
            node_ids: vec![9],
        };
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![node]])
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .append_query_results(vec![vec![row]])
            .into_connection();
        let service = BuildNodePolicyService::new(Arc::new(db));
        let result = service.set(None, Some(vec![9])).await.unwrap();
        assert_eq!(result.node_ids, Some(vec![9]));
        assert_eq!(result.source, BuildNodePolicySource::Global);
    }

    #[tokio::test]
    async fn missing_project_is_not_found() {
        let count =
            std::collections::BTreeMap::from([("num_items", sea_orm::Value::BigInt(Some(0)))]);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![count]])
            .into_connection();
        let service = BuildNodePolicyService::new(Arc::new(db));
        assert!(matches!(
            service.get(Some(99)).await,
            Err(BuildNodePolicyError::ProjectNotFound { project_id: 99 })
        ));
    }

    #[test]
    fn validates_exclusive_candidates() {
        for ids in [vec![], vec![0], vec![-1], vec![1, 1], (1..=101).collect()] {
            assert!(validate_node_ids(&ids).is_err());
        }
        assert!(validate_node_ids(&[2, 1]).is_ok());
    }

    #[test]
    fn clearing_requires_explicit_null() {
        assert!(serde_json::from_str::<SetBuildNodesRequest>("{}").is_err());
        assert!(
            serde_json::from_str::<SetBuildNodesRequest>(r#"{"node_ids":null}"#)
                .unwrap()
                .node_ids
                .is_none()
        );
        assert!(serde_json::from_str::<SetBuildNodesRequest>(
            r#"{"node_ids":[1],"fallback":true}"#
        )
        .is_err());
    }

    #[test]
    fn project_overrides_global_without_combining_pools() {
        let rows = vec![
            policies::Model {
                scope: "global".into(),
                project_id: None,
                node_ids: vec![1, 2],
            },
            policies::Model {
                scope: "project:7".into(),
                project_id: Some(7),
                node_ids: vec![3],
            },
        ];
        let project = resolve_rows(Some(7), &rows);
        assert_eq!(project.effective_node_ids, Some(vec![3]));
        assert_eq!(project.source, BuildNodePolicySource::Project);
        let inherited = resolve_rows(Some(8), &rows);
        assert_eq!(inherited.node_ids, None);
        assert_eq!(inherited.effective_node_ids, Some(vec![1, 2]));
        assert_eq!(inherited.source, BuildNodePolicySource::Global);
        assert!(!project.builds_locally(true));
        assert!(!inherited.builds_locally(true));
        assert!(resolve_rows(Some(7), &[]).builds_locally(true));
        assert!(!resolve_rows(Some(7), &[]).builds_locally(false));
        assert_eq!(resolve_rows(None, &rows).node_ids, Some(vec![1, 2]));
        assert_eq!(
            resolve_rows(Some(7), &[]).source,
            BuildNodePolicySource::Automatic
        );
    }
}
