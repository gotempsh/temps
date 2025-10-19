use chrono::Utc;
use futures::future::BoxFuture;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, PaginatorTrait, QueryFilter,
    QueryOrder, QuerySelect, Set,
};
use std::sync::Arc;
use std::time::Duration;
use temps_core::UtcDateTime;
use temps_entities::{status_incident_updates, status_incidents};
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
    async fn with_retry<F, T>(
        operation_name: &str,
        mut operation: F,
    ) -> Result<T, StatusPageError>
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
                        debug!("{} succeeded after {} attempts", operation_name, attempt + 1);
                    }
                    return Ok(result);
                }
                Err(e) => {
                    // Check if it's a transient error that we should retry
                    let should_retry = match &e {
                        sea_orm::DbErr::ConnectionAcquire(_) |
                        sea_orm::DbErr::Conn(_) => true,
                        sea_orm::DbErr::Query(runtime_err) => {
                            let err_str = runtime_err.to_string();
                            err_str.contains("deadlock")
                                || err_str.contains("timeout")
                                || err_str.contains("connection")
                        },
                        _ => false,
                    };

                    if should_retry && attempt < MAX_RETRIES {
                        warn!(
                            "{} failed (attempt {}), will retry: {:?}",
                            operation_name, attempt + 1, e
                        );
                        last_error = Some(e);
                        continue;
                    }

                    // Non-retryable error or final attempt
                    error!(
                        "{} failed after {} attempts: {:?}",
                        operation_name, attempt + 1, e
                    );
                    return Err(StatusPageError::Database(e));
                }
            }
        }

        // Should not reach here
        Err(StatusPageError::Database(last_error.unwrap_or_else(||
            sea_orm::DbErr::Custom(format!("{} failed after all retry attempts", operation_name))
        )))
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
                Box::pin(async move {
                    incident.insert(db.as_ref()).await
                })
            },
        ).await?;

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
                Box::pin(async move {
                    update.insert(db.as_ref()).await
                })
            },
        ).await?;

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
            _ => return Err(StatusPageError::Validation(
                "Invalid interval. Must be '5min', 'hourly', or 'daily'".to_string()
            )),
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

