// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use chrono::Utc;
use futures::future::BoxFuture;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, PaginatorTrait, QueryFilter,
    QueryOrder, QuerySelect, Set,
};
use std::sync::Arc;
use std::time::Duration;
use temps_core::UtcDateTime;
use temps_entities::{environments, status_incident_updates, status_incidents, status_monitors};
use tokio::time::sleep;
use tracing::{debug, error, warn};

use super::types::{
    CreateIncidentRequest, IncidentResponse, IncidentUpdateResponse, StatusPageError,
    UpdateIncidentStatusRequest,
};

/// Service for managing status page incidents
pub struct IncidentService {
    db: Arc<DatabaseConnection>,
}

impl IncidentService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    /// Execute a database operation with retry logic
    async fn with_retry<F, T>(operation_name: &str, mut operation: F) -> Result<T, StatusPageError>
    where
        F: FnMut() -> BoxFuture<'static, Result<T, sea_orm::DbErr>>,
    {
        const MAX_RETRIES: u32 = 3;
        const INITIAL_DELAY_MS: u64 = 50;

        let mut last_error = None;

        for attempt in 0..=MAX_RETRIES {
            if attempt > 0 {
                let delay = INITIAL_DELAY_MS * (2_u64.pow(attempt - 1));
                debug!(
                    "Retrying {} (attempt {}/{}), waiting {}ms",
                    operation_name, attempt, MAX_RETRIES, delay
                );
                sleep(Duration::from_millis(delay)).await;
            }

            match operation().await {
                Ok(result) => {
                    if attempt > 0 {
                        debug!(
                            "{} succeeded after {} attempts",
                            operation_name,
                            attempt + 1
                        );
                    }
                    return Ok(result);
                }
                Err(e) => {
                    // Check if it's a transient error that we should retry
                    let should_retry = match &e {
                        sea_orm::DbErr::ConnectionAcquire(_) | sea_orm::DbErr::Conn(_) => true,
                        sea_orm::DbErr::Query(runtime_err) => {
                            let err_str = runtime_err.to_string();
                            err_str.contains("deadlock")
                                || err_str.contains("timeout")
                                || err_str.contains("connection")
                        }
                        _ => false,
                    };

                    if should_retry && attempt < MAX_RETRIES {
                        warn!(
                            "{} failed (attempt {}), will retry: {:?}",
                            operation_name,
                            attempt + 1,
                            e
                        );
                        last_error = Some(e);
                        continue;
                    }

                    // Non-retryable error or final attempt
                    error!(
                        "{} failed after {} attempts: {:?}",
                        operation_name,
                        attempt + 1,
                        e
                    );
                    return Err(StatusPageError::Database(e));
                }
            }
        }

        // Should not reach here
        Err(StatusPageError::Database(last_error.unwrap_or_else(|| {
            sea_orm::DbErr::Custom(format!(
                "{} failed after all retry attempts",
                operation_name
            ))
        })))
    }

    /// Create a new incident with retry logic
    pub async fn create_incident(
        &self,
        project_id: i32,
        request: CreateIncidentRequest,
    ) -> Result<IncidentResponse, StatusPageError> {
        // Validate severity
        if !["minor", "major", "critical"].contains(&request.severity.as_str()) {
            return Err(StatusPageError::Validation(
                "Invalid severity. Must be one of: minor, major, critical".to_string(),
            ));
        }

        if let Some(environment_id) = request.environment_id {
            let environment_project_id = environments::Entity::find_by_id(environment_id)
                .select_only()
                .column(environments::Column::ProjectId)
                .into_tuple::<i32>()
                .one(self.db.as_ref())
                .await
                .map_err(|source| StatusPageError::EnvironmentOwnershipLookup {
                    environment_id,
                    project_id,
                    source,
                })?;
            if environment_project_id != Some(project_id) {
                return Err(StatusPageError::EnvironmentNotInProject {
                    environment_id,
                    project_id,
                });
            }
        }

        if let Some(monitor_id) = request.monitor_id {
            let monitor_project_id = status_monitors::Entity::find_by_id(monitor_id)
                .select_only()
                .column(status_monitors::Column::ProjectId)
                .into_tuple::<i32>()
                .one(self.db.as_ref())
                .await
                .map_err(|source| StatusPageError::MonitorOwnershipLookup {
                    monitor_id,
                    project_id,
                    source,
                })?;
            if monitor_project_id != Some(project_id) {
                return Err(StatusPageError::MonitorNotInProject {
                    monitor_id,
                    project_id,
                });
            }
        }

        let incident = status_incidents::ActiveModel {
            project_id: Set(project_id),
            environment_id: Set(request.environment_id),
            monitor_id: Set(request.monitor_id),
            title: Set(request.title.clone()),
            description: Set(request.description.clone()),
            severity: Set(request.severity.clone()),
            status: Set("investigating".to_string()),
            started_at: Set(Utc::now()),
            resolved_at: Set(None),
            ..Default::default()
        };

        let db = self.db.clone();
        let title = request.title.clone();

        // Create incident with retry
        let result = Self::with_retry(
            &format!("create_incident for project {}", project_id),
            || {
                let incident = incident.clone();
                let db = db.clone();
                Box::pin(async move { incident.insert(db.as_ref()).await })
            },
        )
        .await?;

        let incident_id = result.id;

        // Create initial update with retry
        let initial_update = status_incident_updates::ActiveModel {
            incident_id: Set(incident_id),
            status: Set("investigating".to_string()),
            message: Set(format!("Incident created: {}", title)),
            ..Default::default()
        };

        Self::with_retry(
            &format!("create_incident_update for incident {}", incident_id),
            || {
                let update = initial_update.clone();
                let db = db.clone();
                Box::pin(async move { update.insert(db.as_ref()).await })
            },
        )
        .await?;

        Ok(result.into())
    }

    /// Get incident by ID
    pub async fn get_incident(
        &self,
        incident_id: i32,
    ) -> Result<IncidentResponse, StatusPageError> {
        let incident = status_incidents::Entity::find_by_id(incident_id)
            .one(self.db.as_ref())
            .await?
            .ok_or(StatusPageError::NotFound)?;

        Ok(incident.into())
    }

    /// Resolve the project an incident belongs to, for project-access checks
    /// that must run before any other work on by-incident-id routes (which
    /// carry no `project_id` in their path).
    pub async fn get_incident_project_id(&self, incident_id: i32) -> Result<i32, StatusPageError> {
        status_incidents::Entity::find_by_id(incident_id)
            .select_only()
            .column(status_incidents::Column::ProjectId)
            .into_tuple::<i32>()
            .one(self.db.as_ref())
            .await?
            .ok_or(StatusPageError::NotFound)
    }

    /// List incidents for a project
    pub async fn list_incidents(
        &self,
        project_id: i32,
        environment_id: Option<i32>,
        status: Option<String>,
        page: Option<u64>,
        page_size: Option<u64>,
    ) -> Result<(Vec<IncidentResponse>, u64), StatusPageError> {
        let page = page.unwrap_or(1);
        let page_size = std::cmp::min(page_size.unwrap_or(20), 100);

        let mut query = status_incidents::Entity::find()
            .filter(status_incidents::Column::ProjectId.eq(project_id));

        if let Some(env_id) = environment_id {
            query = query.filter(status_incidents::Column::EnvironmentId.eq(env_id));
        }

        if let Some(status_filter) = status {
            query = query.filter(status_incidents::Column::Status.eq(status_filter));
        }

        query = query.order_by_desc(status_incidents::Column::StartedAt);

        let paginator = query.paginate(self.db.as_ref(), page_size);
        let total = paginator.num_items().await?;
        let items = paginator.fetch_page(page - 1).await?;

        Ok((items.into_iter().map(|i| i.into()).collect(), total))
    }

    /// Update incident status with a message
    pub async fn update_incident_status(
        &self,
        incident_id: i32,
        request: UpdateIncidentStatusRequest,
    ) -> Result<IncidentResponse, StatusPageError> {
        // Validate status
        if !["investigating", "identified", "monitoring", "resolved"]
            .contains(&request.status.as_str())
        {
            return Err(StatusPageError::Validation(
                "Invalid status. Must be one of: investigating, identified, monitoring, resolved"
                    .to_string(),
            ));
        }

        let incident = status_incidents::Entity::find_by_id(incident_id)
            .one(self.db.as_ref())
            .await?
            .ok_or(StatusPageError::NotFound)?;

        let mut incident: status_incidents::ActiveModel = incident.into();
        incident.status = Set(request.status.clone());

        // Set resolved_at if status is resolved
        if request.status == "resolved" {
            incident.resolved_at = Set(Some(Utc::now()));
        }

        let result = incident.update(self.db.as_ref()).await?;

        // Create incident update
        let update = status_incident_updates::ActiveModel {
            incident_id: Set(incident_id),
            status: Set(request.status),
            message: Set(request.message),
            ..Default::default()
        };

        update.insert(self.db.as_ref()).await?;

        Ok(result.into())
    }

    /// Get incident updates
    pub async fn get_incident_updates(
        &self,
        incident_id: i32,
    ) -> Result<Vec<IncidentUpdateResponse>, StatusPageError> {
        let updates = status_incident_updates::Entity::find()
            .filter(status_incident_updates::Column::IncidentId.eq(incident_id))
            .order_by_desc(status_incident_updates::Column::CreatedAt)
            .all(self.db.as_ref())
            .await?;

        Ok(updates.into_iter().map(|u| u.into()).collect())
    }

    /// Delete an incident (soft delete by marking as resolved)
    pub async fn delete_incident(&self, incident_id: i32) -> Result<(), StatusPageError> {
        let incident = status_incidents::Entity::find_by_id(incident_id)
            .one(self.db.as_ref())
            .await?
            .ok_or(StatusPageError::NotFound)?;

        let mut incident: status_incidents::ActiveModel = incident.into();
        incident.status = Set("resolved".to_string());
        incident.resolved_at = Set(Some(Utc::now()));

        incident.update(self.db.as_ref()).await?;

        Ok(())
    }

    /// Get active incidents count
    pub async fn get_active_incidents_count(
        &self,
        project_id: i32,
        environment_id: Option<i32>,
    ) -> Result<u64, StatusPageError> {
        let mut query = status_incidents::Entity::find()
            .filter(status_incidents::Column::ProjectId.eq(project_id))
            .filter(status_incidents::Column::Status.ne("resolved"));

        if let Some(env_id) = environment_id {
            query = query.filter(status_incidents::Column::EnvironmentId.eq(env_id));
        }

        let count = query.count(self.db.as_ref()).await?;

        Ok(count)
    }

    /// Get recent incidents (last 30 days)
    pub async fn get_recent_incidents(
        &self,
        project_id: i32,
        environment_id: Option<i32>,
        limit: Option<u64>,
    ) -> Result<Vec<IncidentResponse>, StatusPageError> {
        let limit = std::cmp::min(limit.unwrap_or(10), 50);
        let start_date = Utc::now() - chrono::Duration::days(30);

        let mut query = status_incidents::Entity::find()
            .filter(status_incidents::Column::ProjectId.eq(project_id))
            .filter(status_incidents::Column::StartedAt.gte(start_date));

        if let Some(env_id) = environment_id {
            query = query.filter(status_incidents::Column::EnvironmentId.eq(env_id));
        }

        let incidents = query
            .order_by_desc(status_incidents::Column::StartedAt)
            .limit(limit)
            .all(self.db.as_ref())
            .await?;

        Ok(incidents.into_iter().map(|i| i.into()).collect())
    }

    /// Get bucketed incident data using time_bucket
    pub async fn get_bucketed_incidents(
        &self,
        project_id: i32,
        environment_id: Option<i32>,
        interval: &str, // "5min", "hourly", or "daily"
        start_time: UtcDateTime,
        end_time: UtcDateTime,
    ) -> Result<super::types::IncidentBucketedResponse, StatusPageError> {
        use sea_orm::FromQueryResult;

        #[derive(FromQueryResult)]
        struct IncidentBucketResult {
            bucket: UtcDateTime,
            total_incidents: i64,
            minor_incidents: i64,
            major_incidents: i64,
            critical_incidents: i64,
            resolved_incidents: i64,
            active_incidents: i64,
            avg_resolution_time_minutes: Option<f64>,
        }

        let bucket_interval = match interval {
            "5min" => "5 minutes",
            "hourly" => "1 hour",
            "daily" => "1 day",
            _ => {
                return Err(StatusPageError::Validation(
                    "Invalid interval. Must be '5min', 'hourly', or 'daily'".to_string(),
                ))
            }
        };

        let env_filter = if let Some(env_id) = environment_id {
            format!("AND environment_id = {}", env_id)
        } else {
            String::new()
        };

        let query = format!(
            r#"
            SELECT
                bucket::timestamptz as bucket,
                total_incidents,
                minor_incidents,
                major_incidents,
                critical_incidents,
                resolved_incidents,
                active_incidents,
                avg_resolution_time_minutes
            FROM (
                SELECT
                    time_bucket('{}', started_at) AS bucket,
                    COUNT(*) as total_incidents,
                    COUNT(*) FILTER (WHERE severity = 'minor') as minor_incidents,
                    COUNT(*) FILTER (WHERE severity = 'major') as major_incidents,
                    COUNT(*) FILTER (WHERE severity = 'critical') as critical_incidents,
                    COUNT(*) FILTER (WHERE status = 'resolved') as resolved_incidents,
                    COUNT(*) FILTER (WHERE status != 'resolved') as active_incidents,
                    AVG(
                        CASE
                            WHEN resolved_at IS NOT NULL THEN
                                EXTRACT(EPOCH FROM (resolved_at - started_at)) / 60
                            ELSE NULL
                        END
                    ) as avg_resolution_time_minutes
                FROM status_incidents
                WHERE project_id = $1
                  AND started_at >= $2
                  AND started_at < $3
                  {}
                GROUP BY bucket
            ) sub
            ORDER BY bucket ASC
            "#,
            bucket_interval, env_filter
        );

        let results = status_incidents::Entity::find()
            .from_raw_sql(sea_orm::Statement::from_sql_and_values(
                sea_orm::DatabaseBackend::Postgres,
                &query,
                vec![project_id.into(), start_time.into(), end_time.into()],
            ))
            .into_model::<IncidentBucketResult>()
            .all(self.db.as_ref())
            .await?;

        let buckets = results
            .into_iter()
            .map(|r| super::types::IncidentBucket {
                bucket_start: r.bucket,
                total_incidents: r.total_incidents,
                minor_incidents: r.minor_incidents,
                major_incidents: r.major_incidents,
                critical_incidents: r.critical_incidents,
                resolved_incidents: r.resolved_incidents,
                active_incidents: r.active_incidents,
                avg_resolution_time_minutes: r.avg_resolution_time_minutes,
            })
            .collect();

        Ok(super::types::IncidentBucketedResponse {
            project_id,
            environment_id,
            interval: interval.to_string(),
            buckets,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ActiveModelTrait, Set};
    use temps_database::test_utils::TestDatabase;
    use temps_entities::{environments, projects, status_monitors, upstream_config::UpstreamList};

    async fn create_test_project(db: &Arc<DatabaseConnection>) -> projects::Model {
        // Use nanoseconds for better uniqueness in parallel tests
        let nanos = chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0);
        let slug = format!("test-project-{}", nanos);
        let project = projects::ActiveModel {
            name: Set("Test Project".to_string()),
            slug: Set(slug.clone()),
            directory: Set(slug),
            main_branch: Set("main".to_string()),
            preset: Set(temps_entities::preset::Preset::Nixpacks),
            repo_name: Set("test-repo".to_string()),
            repo_owner: Set("test-owner".to_string()),
            ..Default::default()
        };
        project.insert(db.as_ref()).await.unwrap()
    }

    async fn create_test_environment(
        db: &Arc<DatabaseConnection>,
        project_id: i32,
    ) -> environments::Model {
        let nanos = chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0);
        let slug = format!("incident-env-{nanos}");
        environments::ActiveModel {
            project_id: Set(project_id),
            name: Set(slug.clone()),
            slug: Set(slug.clone()),
            subdomain: Set(slug.clone()),
            host: Set(format!("{slug}.local")),
            upstreams: Set(UpstreamList::default()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .unwrap()
    }

    async fn create_test_monitor(
        db: &Arc<DatabaseConnection>,
        project_id: i32,
        environment_id: i32,
    ) -> status_monitors::Model {
        status_monitors::ActiveModel {
            project_id: Set(project_id),
            environment_id: Set(Some(environment_id)),
            name: Set("Incident monitor".to_string()),
            monitor_type: Set("web".to_string()),
            check_interval_seconds: Set(60),
            is_active: Set(true),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .unwrap()
    }

    fn sample_incident_request() -> CreateIncidentRequest {
        CreateIncidentRequest {
            title: "Test Incident".to_string(),
            description: None,
            severity: "minor".to_string(),
            environment_id: None,
            monitor_id: None,
        }
    }

    #[tokio::test]
    async fn create_incident_rejects_environment_from_another_project() {
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.connection_arc();
        let service = IncidentService::new(db.clone());
        let target_project = create_test_project(&db).await;
        let other_project = create_test_project(&db).await;
        let foreign_environment = create_test_environment(&db, other_project.id).await;
        let mut request = sample_incident_request();
        request.environment_id = Some(foreign_environment.id);

        let result = service.create_incident(target_project.id, request).await;

        assert!(matches!(
            result,
            Err(StatusPageError::EnvironmentNotInProject {
                environment_id,
                project_id,
            }) if environment_id == foreign_environment.id && project_id == target_project.id
        ));
    }

    #[tokio::test]
    async fn create_incident_rejects_missing_environment() {
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.connection_arc();
        let service = IncidentService::new(db.clone());
        let project = create_test_project(&db).await;
        let mut request = sample_incident_request();
        request.environment_id = Some(i32::MAX);

        let result = service.create_incident(project.id, request).await;

        assert!(matches!(
            result,
            Err(StatusPageError::EnvironmentNotInProject {
                environment_id: i32::MAX,
                project_id,
            }) if project_id == project.id
        ));
    }

    #[tokio::test]
    async fn create_incident_rejects_monitor_from_another_project() {
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.connection_arc();
        let service = IncidentService::new(db.clone());
        let target_project = create_test_project(&db).await;
        let other_project = create_test_project(&db).await;
        let foreign_environment = create_test_environment(&db, other_project.id).await;
        let foreign_monitor =
            create_test_monitor(&db, other_project.id, foreign_environment.id).await;
        let mut request = sample_incident_request();
        request.monitor_id = Some(foreign_monitor.id);

        let result = service.create_incident(target_project.id, request).await;

        assert!(matches!(
            result,
            Err(StatusPageError::MonitorNotInProject {
                monitor_id,
                project_id,
            }) if monitor_id == foreign_monitor.id && project_id == target_project.id
        ));
    }

    #[tokio::test]
    async fn create_incident_rejects_missing_monitor() {
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.connection_arc();
        let service = IncidentService::new(db.clone());
        let project = create_test_project(&db).await;
        let mut request = sample_incident_request();
        request.monitor_id = Some(i32::MAX);

        let result = service.create_incident(project.id, request).await;

        assert!(matches!(
            result,
            Err(StatusPageError::MonitorNotInProject {
                monitor_id: i32::MAX,
                project_id,
            }) if project_id == project.id
        ));
    }

    #[tokio::test]
    async fn create_incident_fails_closed_when_environment_lookup_fails() {
        let db = Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
                .append_query_errors([sea_orm::DbErr::Custom(
                    "environment lookup unavailable".to_string(),
                )])
                .into_connection(),
        );
        let service = IncidentService::new(db);
        let mut request = sample_incident_request();
        request.environment_id = Some(11);

        let result = service.create_incident(7, request).await;

        assert!(matches!(
            result,
            Err(StatusPageError::EnvironmentOwnershipLookup {
                environment_id: 11,
                project_id: 7,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn create_incident_fails_closed_when_monitor_lookup_fails() {
        let db = Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
                .append_query_errors([sea_orm::DbErr::Custom(
                    "monitor lookup unavailable".to_string(),
                )])
                .into_connection(),
        );
        let service = IncidentService::new(db);
        let mut request = sample_incident_request();
        request.monitor_id = Some(13);

        let result = service.create_incident(7, request).await;

        assert!(matches!(
            result,
            Err(StatusPageError::MonitorOwnershipLookup {
                monitor_id: 13,
                project_id: 7,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn test_get_incident_project_id() {
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.connection_arc();
        let service = IncidentService::new(db.clone());

        let project = create_test_project(&db).await;
        let created = service
            .create_incident(project.id, sample_incident_request())
            .await
            .unwrap();

        let project_id = service.get_incident_project_id(created.id).await.unwrap();

        assert_eq!(project_id, project.id);
    }

    #[tokio::test]
    async fn test_get_incident_project_id_not_found() {
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.connection_arc();
        let service = IncidentService::new(db.clone());

        let result = service.get_incident_project_id(99999).await;
        assert!(matches!(result, Err(StatusPageError::NotFound)));
    }

    #[tokio::test]
    async fn test_get_incident_not_found() {
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.connection_arc();
        let service = IncidentService::new(db.clone());

        let result = service.get_incident(99999).await;
        assert!(matches!(result, Err(StatusPageError::NotFound)));
    }
}
