//! Log metadata service: manages log_chunks and log_events in the database
//!
//! Provides CRUD operations for chunk metadata and bulk insert for indexable log events.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, DatabaseConnection, EntityTrait, Order, QueryFilter,
    QueryOrder, QuerySelect, Set,
};
use tracing::debug;
use uuid::Uuid;

use crate::error::LogAggregatorError;
use crate::types::{ChunkMeta, LogLevel, LogLine, LogSource};

/// Parameters for querying log events from the TimescaleDB hypertable.
pub struct LogEventsQuery {
    pub project_id: i32,
    pub start_time: DateTime<Utc>,
    pub end_time: DateTime<Utc>,
    pub levels: Vec<LogLevel>,
    pub services: Vec<String>,
    pub deploy_id: Option<i32>,
    pub limit: u64,
}

/// Service for managing log metadata in the database.
///
/// Handles:
/// - Inserting chunk metadata after flush
/// - Inserting ERROR/WARN log events for fast indexed search
/// - Querying chunk metadata for search and retention
pub struct LogMetadataService {
    db: Arc<DatabaseConnection>,
}

impl LogMetadataService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    /// Insert chunk metadata after a successful flush.
    pub async fn insert_chunk_meta(&self, meta: &ChunkMeta) -> Result<(), LogAggregatorError> {
        let model = temps_entities::log_chunks::ActiveModel {
            id: Set(meta.id),
            project_id: Set(meta.project_id),
            env: Set(meta.env.clone()),
            service: Set(meta.service.clone()),
            container_id: Set(meta.container_id.clone()),
            deploy_id: Set(meta.deploy_id),
            node_id: Set(meta.node_id),
            node_name: Set(meta.node_name.clone()),
            started_at: Set(meta.started_at),
            ended_at: Set(meta.ended_at),
            storage_key: Set(meta.storage_key.clone()),
            line_count: Set(meta.line_count),
            compressed_size_bytes: Set(meta.compressed_size_bytes),
            has_errors: Set(meta.has_errors),
            line_offsets: Set(meta.line_offsets.clone()),
        };

        model.insert(self.db.as_ref()).await?;

        debug!(
            chunk_id = %meta.id,
            project_id = %meta.project_id,
            service = meta.service,
            lines = meta.line_count,
            "Inserted chunk metadata"
        );

        Ok(())
    }

    /// Insert indexable (ERROR/WARN) log events for fast search.
    ///
    /// Each event references its chunk_id and line_offset for context retrieval.
    pub async fn insert_log_events(
        &self,
        chunk_id: Uuid,
        _lines: &[LogLine],
        line_offsets: &[(usize, &LogLine)],
    ) -> Result<u64, LogAggregatorError> {
        if line_offsets.is_empty() {
            return Ok(0);
        }

        let mut count = 0u64;
        for (offset, line) in line_offsets {
            if !line.level.is_indexable() {
                continue;
            }

            let model = temps_entities::log_events::ActiveModel {
                time: Set(line.ts),
                project_id: Set(line.project_id),
                service: Set(line.service.clone()),
                env: Set(line.env.clone()),
                level: Set(line.level.to_string()),
                message: Set(line.msg.clone()),
                fields: Set(line.fields.clone()),
                chunk_id: Set(chunk_id),
                line_offset: Set(*offset as i32),
                deploy_id: Set(line.deploy_id),
            };

            model.insert(self.db.as_ref()).await?;
            count += 1;
        }

        debug!(
            chunk_id = %chunk_id,
            count = count,
            "Inserted log events"
        );

        Ok(count)
    }

    /// Convenience method: insert log_events for indexable (ERROR/WARN) lines from a flushed chunk.
    ///
    /// Filters lines to those with indexable levels, computes their offset, and inserts into DB.
    /// Errors are logged but not propagated — this is used in fire-and-forget callbacks.
    pub async fn insert_log_events_from_lines(&self, chunk_id: Uuid, lines: &[LogLine]) {
        let line_offsets: Vec<(usize, &LogLine)> = lines
            .iter()
            .enumerate()
            .filter(|(_, l)| l.level.is_indexable())
            .collect();

        if line_offsets.is_empty() {
            return;
        }

        if let Err(e) = self.insert_log_events(chunk_id, lines, &line_offsets).await {
            tracing::error!(
                chunk_id = %chunk_id,
                error = %e,
                "Failed to insert log_events"
            );
        }
    }

    /// Query chunk metadata for a project within a time range.
    ///
    /// When `deploy_id` is provided, only chunks tagged with that deployment ID
    /// are returned. This is a SQL-level prefilter that skips entire chunks
    /// before they are fetched and decompressed during archive search.
    #[allow(clippy::too_many_arguments)]
    pub async fn find_chunks(
        &self,
        project_id: i32,
        service: Option<&str>,
        start_time: DateTime<Utc>,
        end_time: DateTime<Utc>,
        deploy_id: Option<i32>,
        container_ids: &[String],
        node_ids: &[i32],
    ) -> Result<Vec<ChunkMeta>, LogAggregatorError> {
        let mut condition = Condition::all()
            .add(temps_entities::log_chunks::Column::ProjectId.eq(project_id))
            .add(temps_entities::log_chunks::Column::StartedAt.lte(end_time))
            .add(temps_entities::log_chunks::Column::EndedAt.gte(start_time));

        if let Some(svc) = service {
            condition =
                condition.add(temps_entities::log_chunks::Column::Service.eq(svc.to_string()));
        }

        if let Some(deploy) = deploy_id {
            condition = condition.add(temps_entities::log_chunks::Column::DeployId.eq(deploy));
        }

        // Container prefilter — each chunk belongs to exactly one container, so
        // this skips entire chunks before any file is fetched/decompressed.
        if !container_ids.is_empty() {
            condition = condition
                .add(temps_entities::log_chunks::Column::ContainerId.is_in(container_ids.to_vec()));
        }

        // Node prefilter on node_id.
        if !node_ids.is_empty() {
            condition =
                condition.add(temps_entities::log_chunks::Column::NodeId.is_in(node_ids.to_vec()));
        }

        let chunks = temps_entities::log_chunks::Entity::find()
            .filter(condition)
            .order_by(temps_entities::log_chunks::Column::StartedAt, Order::Desc)
            .all(self.db.as_ref())
            .await?;

        Ok(chunks
            .into_iter()
            .map(|m| ChunkMeta {
                id: m.id,
                project_id: m.project_id,
                env: m.env,
                service: m.service,
                container_id: m.container_id,
                deploy_id: m.deploy_id,
                node_id: m.node_id,
                node_name: m.node_name,
                started_at: m.started_at,
                ended_at: m.ended_at,
                storage_key: m.storage_key,
                line_count: m.line_count,
                compressed_size_bytes: m.compressed_size_bytes,
                has_errors: m.has_errors,
                line_offsets: m.line_offsets,
            })
            .collect())
    }

    /// List the distinct log sources (container + node + service) present in a
    /// project's chunks for a scope. Powers the history filter dropdowns with
    /// the *full* set of containers/nodes/services, independent of the active
    /// container/node/service filter — so they don't collapse to the current
    /// selection. Filters by project + time, and optionally env + deployment.
    pub async fn list_sources(
        &self,
        project_id: i32,
        env: Option<&str>,
        deploy_id: Option<i32>,
        start_time: DateTime<Utc>,
        end_time: DateTime<Utc>,
    ) -> Result<Vec<LogSource>, LogAggregatorError> {
        use sea_orm::FromQueryResult;

        #[derive(FromQueryResult)]
        struct SourceRow {
            container_id: String,
            service: String,
            node_id: Option<i32>,
            node_name: Option<String>,
        }

        let mut condition = Condition::all()
            .add(temps_entities::log_chunks::Column::ProjectId.eq(project_id))
            .add(temps_entities::log_chunks::Column::StartedAt.lte(end_time))
            .add(temps_entities::log_chunks::Column::EndedAt.gte(start_time));

        if let Some(e) = env {
            condition = condition.add(temps_entities::log_chunks::Column::Env.eq(e.to_string()));
        }
        if let Some(d) = deploy_id {
            condition = condition.add(temps_entities::log_chunks::Column::DeployId.eq(d));
        }

        let rows = temps_entities::log_chunks::Entity::find()
            .filter(condition)
            .select_only()
            .column(temps_entities::log_chunks::Column::ContainerId)
            .column(temps_entities::log_chunks::Column::Service)
            .column(temps_entities::log_chunks::Column::NodeId)
            .column(temps_entities::log_chunks::Column::NodeName)
            .distinct()
            .into_model::<SourceRow>()
            .all(self.db.as_ref())
            .await?;

        Ok(rows
            .into_iter()
            .map(|r| LogSource {
                container_id: r.container_id,
                service: r.service,
                node_id: r.node_id,
                node_name: r.node_name,
            })
            .collect())
    }

    /// Query log_events (ERROR/WARN) from the TimescaleDB hypertable.
    pub async fn query_log_events(
        &self,
        params: &LogEventsQuery,
    ) -> Result<Vec<temps_entities::log_events::Model>, LogAggregatorError> {
        let mut condition = Condition::all()
            .add(temps_entities::log_events::Column::ProjectId.eq(params.project_id))
            .add(temps_entities::log_events::Column::Time.gte(params.start_time))
            .add(temps_entities::log_events::Column::Time.lte(params.end_time));

        if !params.levels.is_empty() {
            let level_strings: Vec<String> = params.levels.iter().map(|l| l.to_string()).collect();
            condition =
                condition.add(temps_entities::log_events::Column::Level.is_in(level_strings));
        }

        if !params.services.is_empty() {
            condition = condition
                .add(temps_entities::log_events::Column::Service.is_in(params.services.clone()));
        }

        if let Some(deploy) = params.deploy_id {
            condition = condition.add(temps_entities::log_events::Column::DeployId.eq(deploy));
        }

        let events = temps_entities::log_events::Entity::find()
            .filter(condition)
            .order_by(temps_entities::log_events::Column::Time, Order::Asc)
            .limit(params.limit)
            .all(self.db.as_ref())
            .await?;

        Ok(events)
    }

    /// Find chunks older than a given timestamp for retention cleanup.
    pub async fn find_expired_chunks(
        &self,
        project_id: i32,
        before: DateTime<Utc>,
    ) -> Result<Vec<ChunkMeta>, LogAggregatorError> {
        let chunks = temps_entities::log_chunks::Entity::find()
            .filter(
                Condition::all()
                    .add(temps_entities::log_chunks::Column::ProjectId.eq(project_id))
                    .add(temps_entities::log_chunks::Column::EndedAt.lt(before)),
            )
            .all(self.db.as_ref())
            .await?;

        Ok(chunks
            .into_iter()
            .map(|m| ChunkMeta {
                id: m.id,
                project_id: m.project_id,
                env: m.env,
                service: m.service,
                container_id: m.container_id,
                deploy_id: m.deploy_id,
                node_id: m.node_id,
                node_name: m.node_name,
                started_at: m.started_at,
                ended_at: m.ended_at,
                storage_key: m.storage_key,
                line_count: m.line_count,
                compressed_size_bytes: m.compressed_size_bytes,
                has_errors: m.has_errors,
                line_offsets: m.line_offsets,
            })
            .collect())
    }

    /// Delete a chunk metadata row by ID.
    pub async fn delete_chunk_meta(&self, chunk_id: Uuid) -> Result<(), LogAggregatorError> {
        temps_entities::log_chunks::Entity::delete_by_id(chunk_id)
            .exec(self.db.as_ref())
            .await?;
        Ok(())
    }

    /// List all distinct project IDs that have log_chunks.
    ///
    /// Used by the retention scheduler to enumerate projects for cleanup.
    pub async fn list_distinct_projects(&self) -> Result<Vec<i32>, LogAggregatorError> {
        use sea_orm::FromQueryResult;

        #[derive(FromQueryResult)]
        struct ProjectIdResult {
            project_id: i32,
        }

        let results = temps_entities::log_chunks::Entity::find()
            .select_only()
            .column(temps_entities::log_chunks::Column::ProjectId)
            .group_by(temps_entities::log_chunks::Column::ProjectId)
            .into_model::<ProjectIdResult>()
            .all(self.db.as_ref())
            .await?;

        Ok(results.into_iter().map(|r| r.project_id).collect())
    }

    /// Get the latest `ended_at` timestamp for a specific container.
    ///
    /// Used by the collector on startup to resume streaming from where it left off
    /// instead of replaying the entire container history.
    pub async fn get_latest_chunk_end_for_container(
        &self,
        container_id: &str,
    ) -> Result<Option<DateTime<Utc>>, LogAggregatorError> {
        let chunk = temps_entities::log_chunks::Entity::find()
            .filter(temps_entities::log_chunks::Column::ContainerId.eq(container_id))
            .order_by(temps_entities::log_chunks::Column::EndedAt, Order::Desc)
            .one(self.db.as_ref())
            .await?;

        Ok(chunk.map(|m| m.ended_at))
    }

    /// Get a single chunk metadata by ID.
    pub async fn get_chunk_meta(
        &self,
        chunk_id: Uuid,
    ) -> Result<Option<ChunkMeta>, LogAggregatorError> {
        let chunk = temps_entities::log_chunks::Entity::find_by_id(chunk_id)
            .one(self.db.as_ref())
            .await?;

        Ok(chunk.map(|m| ChunkMeta {
            id: m.id,
            project_id: m.project_id,
            env: m.env,
            service: m.service,
            container_id: m.container_id,
            deploy_id: m.deploy_id,
            node_id: m.node_id,
            node_name: m.node_name,
            started_at: m.started_at,
            ended_at: m.ended_at,
            storage_key: m.storage_key,
            line_count: m.line_count,
            compressed_size_bytes: m.compressed_size_bytes,
            has_errors: m.has_errors,
            line_offsets: m.line_offsets,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use temps_database::test_utils::TestDatabase;

    /// Insert a chunk_meta directly into the DB for testing.
    async fn insert_test_chunk(
        service: &LogMetadataService,
        project_id: i32,
        svc: &str,
        env: &str,
    ) -> ChunkMeta {
        let chunk = ChunkMeta {
            id: Uuid::new_v4(),
            project_id,
            env: env.to_string(),
            service: svc.to_string(),
            container_id: "test-container".to_string(),
            deploy_id: None,
            node_id: None,
            node_name: None,
            started_at: Utc::now() - Duration::minutes(5),
            ended_at: Utc::now(),
            storage_key: format!("test/{}/{}", project_id, Uuid::new_v4()),
            line_count: 10,
            compressed_size_bytes: 512,
            has_errors: false,
            line_offsets: vec![0],
        };
        service.insert_chunk_meta(&chunk).await.unwrap();
        chunk
    }

    // Note: log_events tests removed — the log_events table was removed from the migration.
    // All searches now go through archive_search (chunk files).

    #[tokio::test]
    #[serial_test::serial]
    async fn test_list_distinct_projects() {
        let db = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(_) => {
                println!("Docker/DB not available, skipping test");
                return;
            }
        };

        let service = LogMetadataService::new(db.connection_arc());

        let project_a = 90003;
        let project_b = 90004;

        // Insert chunks for project A (2 chunks) and project B (1 chunk)
        insert_test_chunk(&service, project_a, "api", "prod").await;
        insert_test_chunk(&service, project_a, "worker", "prod").await;
        insert_test_chunk(&service, project_b, "web", "staging").await;

        let projects = service.list_distinct_projects().await.unwrap();

        assert!(projects.contains(&project_a), "Should contain project_a");
        assert!(projects.contains(&project_b), "Should contain project_b");
        // Even though project_a has 2 chunks, it should only appear once
        assert_eq!(
            projects.iter().filter(|p| **p == project_a).count(),
            1,
            "project_a should appear exactly once"
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_list_distinct_projects_empty() {
        let db = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(_) => {
                println!("Docker/DB not available, skipping test");
                return;
            }
        };

        let service = LogMetadataService::new(db.connection_arc());
        let projects = service.list_distinct_projects().await.unwrap();
        assert!(
            projects.is_empty(),
            "Should return empty list when no chunks exist"
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_list_sources_distinct() {
        let db = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(_) => {
                println!("Docker/DB not available, skipping test");
                return;
            }
        };
        let service = LogMetadataService::new(db.connection_arc());
        let project = 90010;
        let now = Utc::now();
        let mk = |cid: &str, node: Option<i32>, nname: Option<&str>| ChunkMeta {
            id: Uuid::new_v4(),
            project_id: project,
            env: "1".to_string(),
            service: "web".to_string(),
            container_id: cid.to_string(),
            deploy_id: Some(1),
            node_id: node,
            node_name: nname.map(|s| s.to_string()),
            started_at: now - Duration::minutes(5),
            ended_at: now,
            storage_key: format!("k/{}", Uuid::new_v4()),
            line_count: 1,
            compressed_size_bytes: 10,
            has_errors: false,
            line_offsets: vec![0],
        };
        // Two chunks for the same remote container (must dedup to one source),
        // plus a second remote node and a control-plane-local container.
        service
            .insert_chunk_meta(&mk("cnt-a", Some(1), Some("worker-1")))
            .await
            .unwrap();
        service
            .insert_chunk_meta(&mk("cnt-a", Some(1), Some("worker-1")))
            .await
            .unwrap();
        service
            .insert_chunk_meta(&mk("cnt-b", Some(2), Some("worker-2")))
            .await
            .unwrap();
        service
            .insert_chunk_meta(&mk("cnt-local", None, None))
            .await
            .unwrap();

        let sources = service
            .list_sources(
                project,
                Some("1"),
                None,
                now - Duration::hours(1),
                now + Duration::hours(1),
            )
            .await
            .unwrap();

        // 3 distinct containers (cnt-a's two chunks collapse to one source).
        assert_eq!(
            sources.len(),
            3,
            "expected 3 distinct sources, got {sources:?}"
        );
        let nodes: std::collections::HashSet<_> =
            sources.iter().filter_map(|s| s.node_id).collect();
        assert!(
            nodes.contains(&1) && nodes.contains(&2),
            "both remote nodes present"
        );
        assert!(
            sources
                .iter()
                .any(|s| s.container_id == "cnt-local" && s.node_id.is_none()),
            "control-plane-local container present with no node_id"
        );
    }
}
