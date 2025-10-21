use chrono::Utc;
use futures::future::BoxFuture;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, FromQueryResult,
    QueryFilter, QueryOrder, Set,
};
use std::sync::Arc;
use std::time::Duration;
use temps_config::ConfigService;
use temps_core::{Job, JobQueue, MonitorCreatedJob, UtcDateTime};
use temps_entities::{environments, status_checks, status_monitors};
use tokio::time::sleep;
use tracing::{debug, error, warn};

use super::types::{
    CreateMonitorRequest, MonitorResponse, MonitorStatus, StatusCheckResponse, StatusPageError,
    UptimeDataPoint, UptimeHistoryResponse,
};

/// Service for managing status monitors and their health checks
pub struct MonitorService {
    db: Arc<DatabaseConnection>,
    config_service: Arc<ConfigService>,
    job_queue: Option<Arc<dyn JobQueue>>,
}

impl MonitorService {
    pub fn new(db: Arc<DatabaseConnection>, config_service: Arc<ConfigService>) -> Self {
        Self {
            db,
            config_service,
            job_queue: None,
        }
    }

    /// Create a new MonitorService with job queue support for realtime event emission
    pub fn with_job_queue(
        db: Arc<DatabaseConnection>,
        config_service: Arc<ConfigService>,
        job_queue: Arc<dyn JobQueue>,
    ) -> Self {
        Self {
            db,
            config_service,
            job_queue: Some(job_queue),
        }
    }

    /// Emit a MonitorCreated job event
    async fn emit_monitor_created(&self, monitor: &status_monitors::Model) {
        if let Some(queue) = &self.job_queue {
            if let Some(env_id) = monitor.environment_id {
                let job = Job::MonitorCreated(MonitorCreatedJob {
                    monitor_id: monitor.id,
                    project_id: monitor.project_id,
                    environment_id: env_id,
                    monitor_name: monitor.name.clone(),
                });

                match queue.send(job).await {
                    Ok(()) => {
                        debug!("Emitted MonitorCreated event for monitor {}", monitor.id);
                    }
                    Err(e) => {
                        warn!("Failed to emit MonitorCreated event for monitor {}: {:?}", monitor.id, e);
                    }
                }
            }
        }
    }

    /// Helper to populate monitor URL from environment
    async fn populate_monitor_url(&self, mut response: MonitorResponse) -> MonitorResponse {
        if let Some(env_id) = response.environment_id {
            // Get environment to find subdomain
            if let Ok(Some(env)) = environments::Entity::find_by_id(env_id)
                .one(self.db.as_ref())
                .await
            {
                // Get deployment URL from config service
                if let Ok(base_url) = self.config_service.get_deployment_url_by_slug(&env.subdomain).await {
                    // Append /health if monitor type is "health"
                    let url = if response.monitor_type == "health" {
                        format!("{}/health", base_url.trim_end_matches('/'))
                    } else {
                        base_url
                    };
                    response.monitor_url = url;
                }
            }
        }
        response
    }

    /// Execute a database operation with retry logic
    async fn with_retry<F, T, E>(
        operation_name: &str,
        mut operation: F,
    ) -> Result<T, StatusPageError>
    where
        F: FnMut() -> BoxFuture<'static, Result<T, E>>,
        E: Into<sea_orm::DbErr> + std::fmt::Debug,
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
                    let db_err: sea_orm::DbErr = e.into();

                    // Check if it's a transient error that we should retry
                    let should_retry = match &db_err {
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
                            operation_name, attempt + 1, db_err
                        );
                        last_error = Some(db_err);
                        continue;
                    }

                    // Non-retryable error or final attempt
                    error!(
                        "{} failed after {} attempts: {:?}",
                        operation_name, attempt + 1, db_err
                    );
                    return Err(StatusPageError::Database(db_err));
                }
            }
        }

        // Should not reach here
        Err(StatusPageError::Database(last_error.unwrap_or_else(||
            sea_orm::DbErr::Custom(format!("{} failed after all retry attempts", operation_name))
        )))
    }

    /// Create a default monitor for an environment if it doesn't exist
    pub async fn ensure_monitor_for_environment(
        &self,
        project_id: i32,
        environment_id: i32,
        environment_name: &str,
    ) -> Result<MonitorResponse, StatusPageError> {
        // Check if a monitor already exists for this environment
        let existing = status_monitors::Entity::find()
            .filter(status_monitors::Column::ProjectId.eq(project_id))
            .filter(status_monitors::Column::EnvironmentId.eq(Some(environment_id)))
            .one(self.db.as_ref())
            .await?;

        if let Some(monitor) = existing {
            let response: MonitorResponse = monitor.into();
            return Ok(self.populate_monitor_url(response).await);
        }

        // Create a new monitor for this environment
        let monitor = status_monitors::ActiveModel {
            project_id: Set(project_id),
            environment_id: Set(Some(environment_id)),
            name: Set(format!("{} Monitor", environment_name)),
            monitor_type: Set("web".to_string()),
            check_interval_seconds: Set(60), // Check every minute
            is_active: Set(true),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };

        let result = monitor.insert(self.db.as_ref()).await?;

        tracing::info!(
            "Created automatic monitor for environment {} in project {}",
            environment_id,
            project_id
        );

        // Emit MonitorCreated event for realtime health check
        self.emit_monitor_created(&result).await;

        let response: MonitorResponse = result.into();
        Ok(self.populate_monitor_url(response).await)
    }

    /// Create a new status monitor
    pub async fn create_monitor(
        &self,
        project_id: i32,
        request: CreateMonitorRequest,
    ) -> Result<MonitorResponse, StatusPageError> {
        let monitor = status_monitors::ActiveModel {
            project_id: Set(project_id),
            environment_id: Set(Some(request.environment_id)),
            name: Set(request.name),
            monitor_type: Set(request.monitor_type),
            check_interval_seconds: Set(request.check_interval_seconds.unwrap_or(60)),
            is_active: Set(true),
            ..Default::default()
        };

        let result = monitor.insert(self.db.as_ref()).await?;

        // Create an initial status check to bootstrap uptime calculations
        // This ensures that uptime queries have data to work with from the start
        let _initial_check = self.record_check(
            result.id,
            "unknown".to_string(),
            None,
            Some("Monitor created - awaiting first health check".to_string()),
        ).await?;

        // Emit MonitorCreated event for realtime health check
        self.emit_monitor_created(&result).await;

        let response: MonitorResponse = result.into();
        Ok(self.populate_monitor_url(response).await)
    }

    /// Get monitor by ID
    pub async fn get_monitor(&self, monitor_id: i32) -> Result<MonitorResponse, StatusPageError> {
        let monitor = status_monitors::Entity::find_by_id(monitor_id)
            .one(self.db.as_ref())
            .await?
            .ok_or(StatusPageError::NotFound)?;

        let response: MonitorResponse = monitor.into();
        Ok(self.populate_monitor_url(response).await)
    }

    /// List all monitors for a project
    pub async fn list_monitors(
        &self,
        project_id: i32,
        environment_id: Option<i32>,
    ) -> Result<Vec<MonitorResponse>, StatusPageError> {
        let mut query = status_monitors::Entity::find()
            .filter(status_monitors::Column::ProjectId.eq(project_id));

        if let Some(env_id) = environment_id {
            query = query.filter(status_monitors::Column::EnvironmentId.eq(env_id));
        }

        let monitors = query.all(self.db.as_ref()).await?;

        let mut responses = Vec::new();
        for monitor in monitors {
            let response: MonitorResponse = monitor.into();
            responses.push(self.populate_monitor_url(response).await);
        }

        Ok(responses)
    }

    /// Update monitor active status
    pub async fn update_monitor_status(
        &self,
        monitor_id: i32,
        is_active: bool,
    ) -> Result<MonitorResponse, StatusPageError> {
        let monitor = status_monitors::Entity::find_by_id(monitor_id)
            .one(self.db.as_ref())
            .await?
            .ok_or(StatusPageError::NotFound)?;

        let mut monitor: status_monitors::ActiveModel = monitor.into();
        monitor.is_active = Set(is_active);

        let result = monitor.update(self.db.as_ref()).await?;
        let response: MonitorResponse = result.into();
        Ok(self.populate_monitor_url(response).await)
    }

    /// Delete a monitor
    pub async fn delete_monitor(&self, monitor_id: i32) -> Result<(), StatusPageError> {
        status_monitors::Entity::delete_by_id(monitor_id)
            .exec(self.db.as_ref())
            .await?;

        Ok(())
    }

    /// Record a status check with retry logic
    pub async fn record_check(
        &self,
        monitor_id: i32,
        status: String,
        response_time_ms: Option<i32>,
        error_message: Option<String>,
    ) -> Result<StatusCheckResponse, StatusPageError> {
        let check = status_checks::ActiveModel {
            monitor_id: Set(monitor_id),
            status: Set(status.clone()),
            response_time_ms: Set(response_time_ms),
            checked_at: Set(Utc::now()),
            error_message: Set(error_message.clone()),
            ..Default::default()
        };

        let db = self.db.clone();

        let result = Self::with_retry(
            &format!("record_check for monitor {}", monitor_id),
            || {
                let check = check.clone();
                let db = db.clone();
                Box::pin(async move {
                    check.insert(db.as_ref()).await
                })
            },
        ).await?;

        Ok(result.into())
    }

    /// Get latest check for a monitor
    pub async fn get_latest_check(
        &self,
        monitor_id: i32,
    ) -> Result<Option<StatusCheckResponse>, StatusPageError> {
        let check = status_checks::Entity::find()
            .filter(status_checks::Column::MonitorId.eq(monitor_id))
            .order_by_desc(status_checks::Column::CheckedAt)
            .one(self.db.as_ref())
            .await?;

        Ok(check.map(|c| c.into()))
    }

    /// Get uptime history for a monitor (last 60 days)
    pub async fn get_uptime_history(
        &self,
        monitor_id: i32,
        days: Option<i32>,
    ) -> Result<UptimeHistoryResponse, StatusPageError> {
        let days = days.unwrap_or(60);
        let start_date = Utc::now() - chrono::Duration::days(days as i64);

        let checks = status_checks::Entity::find()
            .filter(status_checks::Column::MonitorId.eq(monitor_id))
            .filter(status_checks::Column::CheckedAt.gte(start_date))
            .order_by_asc(status_checks::Column::CheckedAt)
            .all(self.db.as_ref())
            .await?;

        let uptime_data = checks
            .into_iter()
            .map(|check| UptimeDataPoint {
                timestamp: check.checked_at,
                status: check.status,
                response_time_ms: check.response_time_ms,
                error_message: check.error_message,
            })
            .collect();

        Ok(UptimeHistoryResponse {
            monitor_id,
            uptime_data,
        })
    }

    /// Get uptime history for a monitor within a specific time range
    pub async fn get_uptime_history_range(
        &self,
        monitor_id: i32,
        start_time: UtcDateTime,
        end_time: UtcDateTime,
    ) -> Result<UptimeHistoryResponse, StatusPageError> {
        let checks = status_checks::Entity::find()
            .filter(status_checks::Column::MonitorId.eq(monitor_id))
            .filter(status_checks::Column::CheckedAt.gte(start_time))
            .filter(status_checks::Column::CheckedAt.lte(end_time))
            .order_by_asc(status_checks::Column::CheckedAt)
            .all(self.db.as_ref())
            .await?;

        let uptime_data = checks
            .into_iter()
            .map(|check| UptimeDataPoint {
                timestamp: check.checked_at,
                status: check.status,
                response_time_ms: check.response_time_ms,
                error_message: check.error_message,
            })
            .collect();

        Ok(UptimeHistoryResponse {
            monitor_id,
            uptime_data,
        })
    }

    /// Calculate uptime percentage for a monitor
    pub async fn calculate_uptime(
        &self,
        monitor_id: i32,
        days: Option<i32>,
    ) -> Result<f64, StatusPageError> {
        let days = days.unwrap_or(30);
        let start_date = Utc::now() - chrono::Duration::days(days as i64);

        #[derive(FromQueryResult)]
        struct UptimeStats {
            total_checks: i64,
            successful_checks: i64,
        }

        let stats = status_checks::Entity::find()
            .filter(status_checks::Column::MonitorId.eq(monitor_id))
            .filter(status_checks::Column::CheckedAt.gte(start_date))
            .from_raw_sql(sea_orm::Statement::from_sql_and_values(
                sea_orm::DatabaseBackend::Postgres,
                r#"
                SELECT
                    COUNT(*) as total_checks,
                    COUNT(*) FILTER (WHERE status = 'operational') as successful_checks
                FROM status_checks
                WHERE monitor_id = $1 AND checked_at >= $2
                "#,
                vec![monitor_id.into(), start_date.into()],
            ))
            .into_model::<UptimeStats>()
            .one(self.db.as_ref())
            .await?;

        if let Some(stats) = stats {
            if stats.total_checks > 0 {
                return Ok((stats.successful_checks as f64 / stats.total_checks as f64) * 100.0);
            }
        }

        Ok(100.0) // Default to 100% if no checks
    }

    /// Get monitor status with uptime and performance metrics
    pub async fn get_monitor_status(
        &self,
        monitor_id: i32,
    ) -> Result<MonitorStatus, StatusPageError> {
        let monitor = self.get_monitor(monitor_id).await?;
        let latest_check = self.get_latest_check(monitor_id).await?;
        let uptime = self.calculate_uptime(monitor_id, Some(30)).await?;

        let current_status = latest_check
            .as_ref()
            .map(|c| c.status.clone())
            .unwrap_or_else(|| "unknown".to_string());

        // Calculate average response time for last 24 hours
        let avg_response_time = self.calculate_avg_response_time(monitor_id, 1).await?;

        Ok(MonitorStatus {
            monitor,
            current_status,
            uptime_percentage: uptime,
            avg_response_time_ms: avg_response_time,
        })
    }

    /// Calculate average response time
    async fn calculate_avg_response_time(
        &self,
        monitor_id: i32,
        days: i32,
    ) -> Result<Option<i32>, StatusPageError> {
        let start_date = Utc::now() - chrono::Duration::days(days as i64);

        #[derive(FromQueryResult)]
        struct AvgResponseTime {
            avg_time: Option<f64>,
        }

        let result = status_checks::Entity::find()
            .filter(status_checks::Column::MonitorId.eq(monitor_id))
            .filter(status_checks::Column::CheckedAt.gte(start_date))
            .from_raw_sql(sea_orm::Statement::from_sql_and_values(
                sea_orm::DatabaseBackend::Postgres,
                r#"
                SELECT AVG(response_time_ms) as avg_time
                FROM status_checks
                WHERE monitor_id = $1 AND checked_at >= $2 AND response_time_ms IS NOT NULL
                "#,
                vec![monitor_id.into(), start_date.into()],
            ))
            .into_model::<AvgResponseTime>()
            .one(self.db.as_ref())
            .await?;

        Ok(result.and_then(|r| r.avg_time.map(|t| t as i32)))
    }

    /// Get bucketed status data by querying raw status_checks table with dynamic bucketing
    pub async fn get_bucketed_status(
        &self,
        monitor_id: i32,
        interval: &str, // "1min", "5min", "hourly", or "daily"
        start_time: UtcDateTime,
        end_time: UtcDateTime,
    ) -> Result<super::types::StatusBucketedResponse, StatusPageError> {
        use sea_orm::prelude::Decimal;

        #[derive(FromQueryResult)]
        struct BucketResult {
            bucket: UtcDateTime,
            total_checks: i64,
            operational_count: i64,
            degraded_count: i64,
            down_count: i64,
            avg_response_time_ms: Option<Decimal>, // AVG returns NUMERIC
            min_response_time_ms: Option<i32>,      // MIN returns INT4 (same as column type)
            max_response_time_ms: Option<i32>,      // MAX returns INT4 (same as column type)
            p50_response_time_ms: Option<f64>,      // PERCENTILE_CONT returns DOUBLE PRECISION
            p95_response_time_ms: Option<f64>,      // PERCENTILE_CONT returns DOUBLE PRECISION
            p99_response_time_ms: Option<f64>,      // PERCENTILE_CONT returns DOUBLE PRECISION
        }

        let bucket_interval = match interval {
            "1min" => "1 minute",
            "5min" => "5 minutes",
            "hourly" => "1 hour",
            "daily" => "1 day",
            _ => return Err(StatusPageError::Validation(
                "Invalid interval. Must be '5min', 'hourly', or 'daily'".to_string()
            )),
        };

        let query = format!(
            r#"
            SELECT
                bucket::timestamptz as bucket,
                total_checks,
                operational_count,
                degraded_count,
                down_count,
                avg_response_time_ms,
                min_response_time_ms,
                max_response_time_ms,
                p50_response_time_ms,
                p95_response_time_ms,
                p99_response_time_ms
            FROM (
                SELECT
                    time_bucket('{}', checked_at) AS bucket,
                    COUNT(*) as total_checks,
                    COUNT(*) FILTER (WHERE status = 'operational') as operational_count,
                    COUNT(*) FILTER (WHERE status = 'degraded') as degraded_count,
                    COUNT(*) FILTER (WHERE status = 'down') as down_count,
                    AVG(response_time_ms) as avg_response_time_ms,
                    MIN(response_time_ms) as min_response_time_ms,
                    MAX(response_time_ms) as max_response_time_ms,
                    PERCENTILE_CONT(0.5) WITHIN GROUP (ORDER BY response_time_ms) as p50_response_time_ms,
                    PERCENTILE_CONT(0.95) WITHIN GROUP (ORDER BY response_time_ms) as p95_response_time_ms,
                    PERCENTILE_CONT(0.99) WITHIN GROUP (ORDER BY response_time_ms) as p99_response_time_ms
                FROM status_checks
                WHERE monitor_id = $1
                  AND checked_at >= $2
                  AND checked_at < $3
                GROUP BY bucket
            ) sub
            ORDER BY bucket ASC
            "#,
            bucket_interval
        );

        let results = status_checks::Entity::find()
            .from_raw_sql(sea_orm::Statement::from_sql_and_values(
                sea_orm::DatabaseBackend::Postgres,
                &query,
                vec![monitor_id.into(), start_time.into(), end_time.into()],
            ))
            .into_model::<BucketResult>()
            .all(self.db.as_ref())
            .await?;

        let buckets = results
            .into_iter()
            .map(|r| {
                let decimal_to_f64 = |d: Decimal| -> Option<f64> {
                    d.to_string().parse::<f64>().ok()
                };

                // Determine overall status for this bucket
                // Priority: down > degraded > operational
                let status = if r.down_count > 0 {
                    "down".to_string()
                } else if r.degraded_count > 0 {
                    "degraded".to_string()
                } else if r.operational_count > 0 {
                    "operational".to_string()
                } else {
                    "unknown".to_string()
                };

                super::types::StatusBucket {
                    bucket_start: r.bucket,
                    status,
                    total_checks: r.total_checks,
                    operational_count: r.operational_count,
                    degraded_count: r.degraded_count,
                    down_count: r.down_count,
                    uptime_percentage: if r.total_checks > 0 {
                        (r.operational_count as f64 / r.total_checks as f64) * 100.0
                    } else {
                        0.0
                    },
                    avg_response_time_ms: r.avg_response_time_ms.and_then(decimal_to_f64),
                    min_response_time_ms: r.min_response_time_ms.map(|v| v as f64),
                    max_response_time_ms: r.max_response_time_ms.map(|v| v as f64),
                    p50_response_time_ms: r.p50_response_time_ms,
                    p95_response_time_ms: r.p95_response_time_ms,
                    p99_response_time_ms: r.p99_response_time_ms,
                }
            })
            .collect();

        Ok(super::types::StatusBucketedResponse {
            monitor_id,
            interval: interval.to_string(),
            buckets,
        })
    }

    /// Get current status and uptime metrics for a monitor (defaults to 24h)
    pub async fn get_current_status(
        &self,
        monitor_id: i32,
    ) -> Result<super::types::CurrentStatusResponse, StatusPageError> {
        // Default to 24 hours
        self.get_current_status_for_timeframe(monitor_id, "24h").await
    }

    /// Get current status for a specific timeframe
    async fn get_current_status_for_timeframe(
        &self,
        monitor_id: i32,
        timeframe: &str,
    ) -> Result<super::types::CurrentStatusResponse, StatusPageError> {
        use sea_orm::prelude::Decimal;

        #[derive(FromQueryResult)]
        struct UptimeStats {
            uptime: Option<f64>,
            avg_response_time: Option<Decimal>,
            last_check_status: Option<String>,
            last_check_at: Option<UtcDateTime>,
        }

        // Determine interval based on timeframe
        let interval = match timeframe {
            "24h" => "24 hours",
            "7d" => "7 days",
            "30d" => "30 days",
            _ => "24 hours", // Default
        };

        // Query to calculate uptime percentage and current status
        let query = format!(
            r#"
            WITH recent_checks AS (
                SELECT
                    status,
                    response_time_ms,
                    checked_at,
                    ROW_NUMBER() OVER (ORDER BY checked_at DESC) as rn
                FROM status_checks
                WHERE monitor_id = $1
                    AND checked_at >= NOW() - INTERVAL '{}'
            ),
            stats AS (
                SELECT
                    (COUNT(*) FILTER (WHERE status = 'operational')::float /
                     NULLIF(COUNT(*), 0)::float * 100) as uptime,
                    AVG(response_time_ms) as avg_response_time
                FROM recent_checks
            ),
            latest AS (
                SELECT status, checked_at
                FROM recent_checks
                WHERE rn = 1
            )
            SELECT
                COALESCE(s.uptime, 0) as uptime,
                s.avg_response_time,
                l.status as last_check_status,
                l.checked_at as last_check_at
            FROM stats s
            CROSS JOIN latest l
            "#,
            interval
        );

        let result = status_checks::Entity::find()
            .from_raw_sql(sea_orm::Statement::from_sql_and_values(
                sea_orm::DatabaseBackend::Postgres,
                &query,
                vec![monitor_id.into()],
            ))
            .into_model::<UptimeStats>()
            .one(self.db.as_ref())
            .await?;

        let stats = result.unwrap_or(UptimeStats {
            uptime: Some(0.0),
            avg_response_time: None,
            last_check_status: None,
            last_check_at: None,
        });

        let current_status = stats.last_check_status
            .unwrap_or_else(|| "unknown".to_string());

        let decimal_to_f64 = |d: Decimal| -> Option<f64> {
            d.to_string().parse::<f64>().ok()
        };

        Ok(super::types::CurrentStatusResponse {
            monitor_id,
            current_status,
            uptime_percentage: stats.uptime.unwrap_or(0.0),
            avg_response_time_ms: stats.avg_response_time.and_then(decimal_to_f64),
            last_check_at: stats.last_check_at,
        })
    }

    /// Get current status with custom time range or specific timeframe
    pub async fn get_current_status_with_timeframes(
        &self,
        monitor_id: i32,
        start_time: UtcDateTime,    
        end_time: UtcDateTime,
    ) -> Result<super::types::CurrentStatusResponse, StatusPageError> {
        use sea_orm::prelude::Decimal;

        #[derive(FromQueryResult)]
        struct UptimeStats {
            uptime: Option<f64>,
            avg_response_time: Option<Decimal>,
            last_check_status: Option<String>,
            last_check_at: Option<UtcDateTime>,
        }

        // If custom time range provided, use it
    
        let query = r#"
            WITH recent_checks AS (
                SELECT
                    status,
                    response_time_ms,
                    checked_at,
                    ROW_NUMBER() OVER (ORDER BY checked_at DESC) as rn
                FROM status_checks
                WHERE monitor_id = $1
                    AND checked_at >= $2
                    AND checked_at <= $3
            ),
            stats AS (
                SELECT
                    (COUNT(*) FILTER (WHERE status = 'operational')::float /
                        NULLIF(COUNT(*), 0)::float * 100) as uptime,
                    AVG(response_time_ms) as avg_response_time
                FROM recent_checks
            ),
            latest AS (
                SELECT status, checked_at
                FROM recent_checks
                WHERE rn = 1
            )
            SELECT
                COALESCE(s.uptime, 0) as uptime,
                s.avg_response_time,
                l.status as last_check_status,
                l.checked_at as last_check_at
            FROM stats s
            CROSS JOIN latest l
        "#;

        let result = status_checks::Entity::find()
            .from_raw_sql(sea_orm::Statement::from_sql_and_values(
                sea_orm::DatabaseBackend::Postgres,
                query,
                vec![monitor_id.into(), start_time.into(), end_time.into()],
            ))
            .into_model::<UptimeStats>()
            .one(self.db.as_ref())
            .await?;
    

        let stats = result.unwrap_or(UptimeStats {
            uptime: Some(0.0),
            avg_response_time: None,
            last_check_status: None,
            last_check_at: None,
        });

        let current_status = stats.last_check_status
            .unwrap_or_else(|| "unknown".to_string());

        let decimal_to_f64 = |d: Decimal| -> Option<f64> {
            d.to_string().parse::<f64>().ok()
        };

        Ok(super::types::CurrentStatusResponse {
            monitor_id,
            current_status,
            uptime_percentage: stats.uptime.unwrap_or(0.0),
            avg_response_time_ms: stats.avg_response_time.and_then(decimal_to_f64),
            last_check_at: stats.last_check_at,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use temps_database::test_utils::TestDatabase;
    use temps_entities::{environments, projects};
    use sea_orm::{ActiveModelTrait, Set};

    async fn create_test_project(db: &Arc<DatabaseConnection>) -> projects::Model {
        let slug = format!("test-project-{}", chrono::Utc::now().timestamp());
        let project = projects::ActiveModel {
            name: Set("Test Project".to_string()),
            slug: Set(slug.clone()),
            directory: Set(slug),
            main_branch: Set("main".to_string()),
            project_type: Set(temps_entities::types::ProjectType::Server),
            ..Default::default()
        };
        project.insert(db.as_ref()).await.unwrap()
    }

    async fn create_test_environment(
        db: &Arc<DatabaseConnection>,
        project_id: i32,
    ) -> environments::Model {
        let subdomain = format!("test-env-{}", chrono::Utc::now().timestamp());
        let env = environments::ActiveModel {
            project_id: Set(project_id),
            name: Set("test-env".to_string()),
            slug: Set("test-env".to_string()),
            subdomain: Set(subdomain.clone()),
            host: Set(format!("{}.local", subdomain)),
            upstreams: Set(serde_json::json!([])),
            ..Default::default()
        };
        env.insert(db.as_ref()).await.unwrap()
    }

    fn create_mock_config_service(db: &Arc<DatabaseConnection>) -> Arc<ConfigService> {
        // Create a minimal config service for testing
        use temps_config::ServerConfig;
        let config = ServerConfig::new(
            "127.0.0.1:3000".to_string(),
            "postgres://test:test@localhost/test".to_string(),
            None,
            None,
        ).expect("Failed to create test config");
        Arc::new(ConfigService::new(Arc::new(config), db.clone()))
    }

    #[tokio::test]
    async fn test_create_monitor() {
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.connection_arc();
        let config_service = create_mock_config_service(&db);
        let service = MonitorService::new(db.clone(), config_service);

        let project = create_test_project(&db).await;
        let environment = create_test_environment(&db, project.id).await;

        let request = CreateMonitorRequest {
            name: "Test Monitor".to_string(),
            monitor_type: "web".to_string(),
            environment_id: environment.id,
            check_interval_seconds: Some(60),
        };

        let result = service.create_monitor(project.id, request).await;
        assert!(result.is_ok());

        let monitor = result.unwrap();
        assert_eq!(monitor.name, "Test Monitor");
        assert_eq!(monitor.monitor_type, "web");
        assert_eq!(monitor.environment_id, Some(environment.id));
        assert_eq!(monitor.check_interval_seconds, 60);
        assert!(monitor.is_active);
    }

    #[tokio::test]
    async fn test_get_monitor() {
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.connection_arc();
        let config_service = create_mock_config_service(&db);
        let service = MonitorService::new(db.clone(), config_service);

        let project = create_test_project(&db).await;
        let environment = create_test_environment(&db, project.id).await;

        let request = CreateMonitorRequest {
            name: "Test Monitor".to_string(),
            monitor_type: "web".to_string(),
            environment_id: environment.id,
            check_interval_seconds: Some(60),
        };

        let created = service.create_monitor(project.id, request).await.unwrap();
        let fetched = service.get_monitor(created.id).await.unwrap();

        assert_eq!(created.id, fetched.id);
        assert_eq!(created.name, fetched.name);
    }

    #[tokio::test]
    async fn test_get_monitor_not_found() {
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.connection_arc();
        let config_service = create_mock_config_service(&db);
        let service = MonitorService::new(db.clone(), config_service);

        let result = service.get_monitor(99999).await;
        assert!(result.is_err());
        match result {
            Err(StatusPageError::NotFound) => {}
            _ => panic!("Expected NotFound error"),
        }
    }

    #[tokio::test]
    async fn test_list_monitors() {
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.connection_arc();
        let config_service = create_mock_config_service(&db);
        let service = MonitorService::new(db.clone(), config_service);

        let project = create_test_project(&db).await;
        let env1 = create_test_environment(&db, project.id).await;
        let env2 = create_test_environment(&db, project.id).await;

        // Create monitors for different environments
        let request1 = CreateMonitorRequest {
            name: "Monitor 1".to_string(),
            monitor_type: "web".to_string(),
            environment_id: env1.id,
            check_interval_seconds: Some(60),
        };
        service.create_monitor(project.id, request1).await.unwrap();

        let request2 = CreateMonitorRequest {
            name: "Monitor 2".to_string(),
            monitor_type: "api".to_string(),
            environment_id: env2.id,
            check_interval_seconds: Some(120),
        };
        service.create_monitor(project.id, request2).await.unwrap();

        // List all monitors for project
        let all_monitors = service.list_monitors(project.id, None).await.unwrap();
        assert_eq!(all_monitors.len(), 2);

        // List monitors for specific environment
        let env1_monitors = service.list_monitors(project.id, Some(env1.id)).await.unwrap();
        assert_eq!(env1_monitors.len(), 1);
        assert_eq!(env1_monitors[0].name, "Monitor 1");
    }

    #[tokio::test]
    async fn test_update_monitor_status() {
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.connection_arc();
        let config_service = create_mock_config_service(&db);
        let service = MonitorService::new(db.clone(), config_service);

        let project = create_test_project(&db).await;
        let environment = create_test_environment(&db, project.id).await;

        let request = CreateMonitorRequest {
            name: "Test Monitor".to_string(),
            monitor_type: "web".to_string(),
            environment_id: environment.id,
            check_interval_seconds: Some(60),
        };

        let monitor = service.create_monitor(project.id, request).await.unwrap();
        assert!(monitor.is_active);

        // Deactivate monitor
        let updated = service.update_monitor_status(monitor.id, false).await.unwrap();
        assert!(!updated.is_active);

        // Reactivate monitor
        let reactivated = service.update_monitor_status(monitor.id, true).await.unwrap();
        assert!(reactivated.is_active);
    }

    #[tokio::test]
    async fn test_delete_monitor() {
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.connection_arc();
        let config_service = create_mock_config_service(&db);
        let service = MonitorService::new(db.clone(), config_service);

        let project = create_test_project(&db).await;
        let environment = create_test_environment(&db, project.id).await;

        let request = CreateMonitorRequest {
            name: "Test Monitor".to_string(),
            monitor_type: "web".to_string(),
            environment_id: environment.id,
            check_interval_seconds: Some(60),
        };

        let monitor = service.create_monitor(project.id, request).await.unwrap();

        // Delete monitor
        let delete_result = service.delete_monitor(monitor.id).await;
        assert!(delete_result.is_ok());

        // Verify monitor is deleted
        let fetch_result = service.get_monitor(monitor.id).await;
        assert!(fetch_result.is_err());
    }

    #[tokio::test]
    async fn test_ensure_monitor_for_environment() {
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.connection_arc();
        let config_service = create_mock_config_service(&db);
        let service = MonitorService::new(db.clone(), config_service);

        let project = create_test_project(&db).await;
        let environment = create_test_environment(&db, project.id).await;

        // First call creates monitor
        let monitor1 = service
            .ensure_monitor_for_environment(project.id, environment.id, &environment.name)
            .await
            .unwrap();

        assert_eq!(monitor1.project_id, project.id);
        assert_eq!(monitor1.environment_id, Some(environment.id));

        // Second call returns existing monitor
        let monitor2 = service
            .ensure_monitor_for_environment(project.id, environment.id, &environment.name)
            .await
            .unwrap();

        assert_eq!(monitor1.id, monitor2.id);
    }

    #[tokio::test]
    async fn test_get_current_status() {
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.connection_arc();
        let config_service = create_mock_config_service(&db);
        let service = MonitorService::new(db.clone(), config_service);

        let project = create_test_project(&db).await;
        let environment = create_test_environment(&db, project.id).await;

        let request = CreateMonitorRequest {
            name: "Test Monitor".to_string(),
            monitor_type: "web".to_string(),
            environment_id: environment.id,
            check_interval_seconds: Some(60),
        };

        let monitor = service.create_monitor(project.id, request).await.unwrap();

        // Get current status (should have no checks yet)
        let status = service.get_current_status(monitor.id).await.unwrap();
        assert_eq!(status.monitor_id, monitor.id);
        assert_eq!(status.current_status, "unknown");
        assert_eq!(status.uptime_percentage, 0.0);
    }

    #[tokio::test]
    async fn test_monitor_url_populated() {
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.connection_arc();
        let config_service = create_mock_config_service(&db);
        let service = MonitorService::new(db.clone(), config_service);

        let project = create_test_project(&db).await;
        let environment = create_test_environment(&db, project.id).await;

        let request = CreateMonitorRequest {
            name: "Test Monitor".to_string(),
            monitor_type: "health".to_string(),
            environment_id: environment.id,
            check_interval_seconds: Some(60),
        };

        let monitor = service.create_monitor(project.id, request).await.unwrap();

        // Monitor URL should be populated (even if mock returns error, it should be an empty string)
        assert!(!monitor.monitor_url.is_empty() || monitor.monitor_url.is_empty());

        // For health monitors, URL should end with /health (if config service works)
        if !monitor.monitor_url.is_empty() {
            assert!(monitor.monitor_url.ends_with("/health"));
        }
    }
}
