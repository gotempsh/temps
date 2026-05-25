use chrono::Utc;
use sea_orm::{ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, Set};
use std::sync::Arc;
use std::time::Duration;
use temps_config::ConfigService;
use temps_core::{Job, JobQueue, JobReceiver, StatusCheckCompletedJob};
use temps_entities::{
    deployment_containers, deployments, environments, projects, status_checks, status_monitors,
};
use tokio::time::{sleep, timeout};
use tracing::{debug, error, info, warn};

use super::types::{validate_check_path, StatusPageError};

/// Service for performing health checks on monitored environments
pub struct HealthCheckService {
    db: Arc<DatabaseConnection>,
    http_client: reqwest::Client,
    config_service: Arc<ConfigService>,
    job_queue: Arc<dyn JobQueue>,
}

impl HealthCheckService {
    /// Create a new HealthCheckService with mandatory ConfigService and JobQueue
    pub fn new(
        db: Arc<DatabaseConnection>,
        config_service: Arc<ConfigService>,
        job_queue: Arc<dyn JobQueue>,
    ) -> Self {
        let http_client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent("Temps-Status-Monitor/1.0")
            .build()
            .expect("Failed to create HTTP client");

        Self {
            db,
            http_client,
            config_service,
            job_queue,
        }
    }

    /// Run health checks for all active monitors
    pub async fn run_all_checks(&self) -> Result<(), StatusPageError> {
        debug!("Starting health check cycle");

        // Single query: join monitors with environments to skip on-demand ones.
        // Health checks go through the proxy, which resets the idle timer and
        // would prevent scale-to-zero from ever triggering.
        let monitors_with_envs = status_monitors::Entity::find()
            .filter(status_monitors::Column::IsActive.eq(true))
            .find_also_related(environments::Entity)
            .all(self.db.as_ref())
            .await?;

        let total_monitors = monitors_with_envs.len();
        debug!("Found {} active monitors to check", total_monitors);

        let filtered_monitors: Vec<_> = Self::filter_on_demand_monitors(monitors_with_envs);

        debug!(
            "Running checks for {} monitors ({} skipped as on-demand)",
            filtered_monitors.len(),
            total_monitors - filtered_monitors.len()
        );

        // Run checks concurrently with a limit
        let semaphore = Arc::new(tokio::sync::Semaphore::new(10)); // Limit concurrent checks
        let mut tasks = Vec::new();

        for monitor in filtered_monitors {
            let db = self.db.clone();
            let http_client = self.http_client.clone();
            let config_service = self.config_service.clone();
            let job_queue = self.job_queue.clone();
            let permit = semaphore.clone().acquire_owned().await.unwrap();

            let task = tokio::spawn(async move {
                let _permit = permit; // Hold permit until task completes
                if let Err(e) =
                    Self::check_monitor(db, http_client, config_service, monitor, job_queue).await
                {
                    error!("Health check failed: {:?}", e);
                }
            });

            tasks.push(task);
        }

        // Wait for all checks to complete
        for task in tasks {
            if let Err(e) = task.await {
                error!("Task failed: {:?}", e);
            }
        }

        debug!("Health check cycle completed");
        Ok(())
    }

    /// Check a single monitor
    async fn check_monitor(
        db: Arc<DatabaseConnection>,
        http_client: reqwest::Client,
        config_service: Arc<ConfigService>,
        monitor: status_monitors::Model,
        job_queue: Arc<dyn JobQueue>,
    ) -> Result<(), StatusPageError> {
        // Check if environment_id is set
        let env_id = monitor.environment_id.ok_or_else(|| {
            warn!("Monitor {} has no environment_id", monitor.id);
            StatusPageError::InvalidRequest("Monitor has no environment_id".to_string())
        })?;

        debug!("Checking monitor {} for environment {}", monitor.id, env_id);

        // Get the environment to find its deployment URL
        let environment = environments::Entity::find_by_id(env_id)
            .one(db.as_ref())
            .await?
            .ok_or_else(|| StatusPageError::NotFound)?;
        if environment.current_deployment_id.is_none() {
            warn!("Environment {} has no current deployment", env_id);
            return Ok(());
        }

        // IMPORTANT: Always use the public URL for health checks
        // This ensures we're testing the actual user-facing endpoint, not internal container networking
        let health_url = match config_service
            .get_deployment_url_by_slug(&environment.subdomain)
            .await
        {
            Ok(public_url) => {
                debug!("Using public URL for health check: {}", public_url);
                // Use custom check_path if set, otherwise fall back to monitor_type logic.
                // Defense-in-depth: re-validate the stored path at use time so that any
                // rows written before write-time validation was added (or written by a
                // future migration/import path) cannot inject a manipulated URL.
                let base = public_url.trim_end_matches('/');
                match &monitor.check_path {
                    Some(path) if !path.is_empty() && path != "/" => {
                        if let Err(e) = validate_check_path(path) {
                            warn!(
                                monitor_id = monitor.id,
                                check_path = %path,
                                error = %e,
                                "Stored check_path failed validation; falling back to default URL"
                            );
                            public_url
                        } else {
                            // Path is guaranteed to start with '/' by validate_check_path.
                            format!("{}{}", base, path)
                        }
                    }
                    _ if monitor.monitor_type == "health" => {
                        format!("{}/health", base)
                    }
                    _ => public_url,
                }
            }
            Err(e) => {
                error!(
                    "Failed to get public URL for deployment {}: {:?}",
                    environment.subdomain, e
                );

                // Record check as failed due to configuration error
                Self::record_check(
                    &db,
                    monitor.id,
                    "degraded".to_string(),
                    None,
                    Some(format!("Failed to determine public URL: {:?}", e)),
                    &job_queue,
                )
                .await?;

                return Ok(());
            }
        };

        debug!("Checking URL: {}", health_url);

        // Perform the health check with retry logic
        let start_time = std::time::Instant::now();
        let mut last_error = None;
        let mut total_response_time_ms = 0i32;

        // Retry configuration
        const MAX_RETRIES: u32 = 3;
        const INITIAL_DELAY_MS: u64 = 100;
        const MAX_DELAY_MS: u64 = 2000;

        for attempt in 0..=MAX_RETRIES {
            if attempt > 0 {
                // Exponential backoff: 100ms, 200ms, 400ms, 800ms (capped at 2000ms)
                let delay =
                    std::cmp::min(INITIAL_DELAY_MS * (2_u64.pow(attempt - 1)), MAX_DELAY_MS);
                debug!(
                    "Retrying health check for monitor {} (attempt {}/{}), waiting {}ms",
                    monitor.id, attempt, MAX_RETRIES, delay
                );
                sleep(Duration::from_millis(delay)).await;
            }

            let check_result =
                timeout(Duration::from_secs(10), http_client.get(&health_url).send()).await;

            total_response_time_ms = start_time.elapsed().as_millis() as i32;

            match check_result {
                Ok(Ok(response)) => {
                    let status_code = response.status();

                    let status = if status_code.is_success()
                        || status_code.as_u16() == 404
                        || status_code.as_u16() == 405
                    {
                        // 2xx, 404, and 405 are all considered healthy — many apps
                        // return 404 on / (API backends) or 405 (no GET handler)
                        // but the server is running fine.
                        "operational"
                    } else if status_code.is_server_error() {
                        // For server errors, retry
                        if attempt < MAX_RETRIES {
                            last_error =
                                Some(format!("HTTP {} (attempt {})", status_code, attempt + 1));
                            continue;
                        }
                        "major_outage"
                    } else if status_code.is_client_error() {
                        "degraded"
                    } else {
                        "partial_outage"
                    };

                    debug!(
                        "Monitor {} check completed: {} ({}ms, {} attempts)",
                        monitor.id,
                        status,
                        total_response_time_ms,
                        attempt + 1
                    );

                    return Self::record_check(
                        &db,
                        monitor.id,
                        status.to_string(),
                        Some(total_response_time_ms),
                        if status != "operational" {
                            Some(format!(
                                "HTTP {} (after {} attempts)",
                                status_code,
                                attempt + 1
                            ))
                        } else if attempt > 0 {
                            Some(format!("Succeeded after {} attempts", attempt + 1))
                        } else {
                            None
                        },
                        &job_queue,
                    )
                    .await;
                }
                Ok(Err(e)) => {
                    // Only retry on timeouts — connection refused means the container is down,
                    // retrying immediately just generates noise without any chance of success.
                    if e.is_timeout() && attempt < MAX_RETRIES {
                        last_error =
                            Some(format!("Request timeout: {} (attempt {})", e, attempt + 1));
                        continue;
                    }

                    // Non-retryable error or final attempt
                    warn!(
                        "Health check request failed for monitor {} after {} attempts: {:?}",
                        monitor.id,
                        attempt + 1,
                        e
                    );

                    let error_msg = if e.is_connect() {
                        "Connection failed"
                    } else if e.is_timeout() {
                        "Request timeout"
                    } else if e.is_redirect() {
                        "Too many redirects"
                    } else {
                        "Request failed"
                    };

                    return Self::record_check(
                        &db,
                        monitor.id,
                        "major_outage".to_string(),
                        Some(total_response_time_ms),
                        Some(format!(
                            "{}: {} (after {} attempts)",
                            error_msg,
                            e,
                            attempt + 1
                        )),
                        &job_queue,
                    )
                    .await;
                }
                Err(_) => {
                    // Timeout - retry
                    if attempt < MAX_RETRIES {
                        last_error =
                            Some(format!("Health check timeout (attempt {})", attempt + 1));
                        continue;
                    }

                    warn!(
                        "Health check timeout for monitor {} after {} attempts",
                        monitor.id,
                        attempt + 1
                    );

                    return Self::record_check(
                        &db,
                        monitor.id,
                        "major_outage".to_string(),
                        Some(10000), // Max timeout
                        Some(format!(
                            "Health check timeout after {} attempts",
                            attempt + 1
                        )),
                        &job_queue,
                    )
                    .await;
                }
            }
        }

        // Should not reach here, but handle it gracefully
        error!("Unexpected: exhausted retries for monitor {}", monitor.id);
        Self::record_check(
            &db,
            monitor.id,
            "major_outage".to_string(),
            Some(total_response_time_ms),
            Some(last_error.unwrap_or_else(|| "Unknown error after retries".to_string())),
            &job_queue,
        )
        .await
    }

    /// Record a check result in the database with retry logic and emit job for outage detection
    async fn record_check(
        db: &Arc<DatabaseConnection>,
        monitor_id: i32,
        status: String,
        response_time_ms: Option<i32>,
        error_message: Option<String>,
        job_queue: &Arc<dyn JobQueue>,
    ) -> Result<(), StatusPageError> {
        let check = status_checks::ActiveModel {
            monitor_id: Set(monitor_id),
            status: Set(status.clone()),
            response_time_ms: Set(response_time_ms),
            checked_at: Set(Utc::now()),
            error_message: Set(error_message.clone()),
            ..Default::default()
        };

        // Retry configuration for database operations
        const MAX_DB_RETRIES: u32 = 3;
        const INITIAL_DB_DELAY_MS: u64 = 50;

        let mut last_error = None;

        for attempt in 0..=MAX_DB_RETRIES {
            if attempt > 0 {
                let delay = INITIAL_DB_DELAY_MS * (2_u64.pow(attempt - 1));
                debug!(
                    "Retrying database insert for monitor {} (attempt {}/{}), waiting {}ms",
                    monitor_id, attempt, MAX_DB_RETRIES, delay
                );
                sleep(Duration::from_millis(delay)).await;
            }

            match check.clone().insert(db.as_ref()).await {
                Ok(_) => {
                    if attempt > 0 {
                        debug!("Database insert succeeded after {} attempts", attempt + 1);
                    }

                    // CRITICAL: Emit job for outage detection immediately after recording check
                    let job = Job::StatusCheckCompleted(StatusCheckCompletedJob {
                        monitor_id,
                        status: status.clone(),
                        error_message: error_message.clone(),
                    });

                    if let Err(e) = job_queue.send(job).await {
                        error!(
                            "Failed to emit StatusCheckCompleted job for monitor {}: {:?}",
                            monitor_id, e
                        );
                        // Don't fail the health check if job emission fails
                    }

                    return Ok(());
                }
                Err(e) => {
                    // Check if it's a transient error that we should retry
                    let should_retry = match &e {
                        sea_orm::DbErr::ConnectionAcquire(_) | sea_orm::DbErr::Conn(_) => true,
                        sea_orm::DbErr::Query(runtime_err) => {
                            let err_str = runtime_err.to_string();
                            err_str.contains("deadlock") || err_str.contains("timeout")
                        }
                        _ => false,
                    };

                    if should_retry && attempt < MAX_DB_RETRIES {
                        warn!(
                            "Database insert failed for monitor {} (attempt {}), will retry: {:?}",
                            monitor_id,
                            attempt + 1,
                            e
                        );
                        last_error = Some(e);
                        continue;
                    }

                    // Non-retryable error or final attempt
                    error!(
                        "Failed to record check for monitor {} after {} attempts: {:?}",
                        monitor_id,
                        attempt + 1,
                        e
                    );
                    return Err(StatusPageError::Database(e));
                }
            }
        }

        // Should not reach here, but handle it
        Err(StatusPageError::Database(last_error.unwrap_or_else(|| {
            sea_orm::DbErr::Custom("Failed after all retry attempts".to_string())
        })))
    }

    /// Initialize monitors for all existing environments
    pub async fn initialize_monitors(&self) -> Result<(), StatusPageError> {
        debug!("Initializing monitors for all existing environments");

        // Get all active (non-deleted) environments with their projects
        let environments_with_projects = environments::Entity::find()
            .filter(environments::Column::DeletedAt.is_null())
            .inner_join(projects::Entity)
            .all(self.db.as_ref())
            .await?;

        let monitor_service = super::monitor_service::MonitorService::new(
            self.db.clone(),
            self.config_service.clone(),
        );

        for env in environments_with_projects {
            match monitor_service
                .ensure_monitor_for_environment(env.project_id, env.id, &env.name)
                .await
            {
                Ok(monitor) => {
                    debug!(
                        "Ensured monitor {} for environment {} ({})",
                        monitor.id, env.id, env.name
                    );
                }
                Err(e) => {
                    warn!(
                        "Failed to create monitor for environment {} ({}): {:?}",
                        env.id, env.name, e
                    );
                }
            }
        }

        debug!("Monitor initialization completed");
        Ok(())
    }

    /// Start the periodic health check scheduler with realtime monitor creation handling
    ///
    /// This scheduler:
    /// 1. Initializes monitors for all existing environments at startup
    /// 2. Runs health checks every 60 seconds for all active monitors
    /// 3. Listens for MonitorCreated events and immediately checks new monitors
    ///
    /// The job_receiver parameter allows the scheduler to react to monitor creation
    /// events in realtime, ensuring new monitors are checked immediately without
    /// waiting for the next scheduled cycle.
    pub async fn start_scheduler(self: Arc<Self>, mut job_receiver: Box<dyn JobReceiver>) {
        debug!("Starting health check scheduler with realtime monitor creation handling");

        // Initialize monitors for all environments first
        if let Err(e) = self.initialize_monitors().await {
            error!("Failed to initialize monitors: {:?}", e);
        }

        // Start the periodic check cycle
        let service_for_interval = self.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(60));
            loop {
                interval.tick().await;
                let service = service_for_interval.clone();
                tokio::spawn(async move {
                    if let Err(e) = service.run_all_checks().await {
                        error!("Health check cycle failed: {:?}", e);
                    }
                });
            }
        });

        // Listen for MonitorCreated events and check new monitors immediately
        loop {
            match job_receiver.recv().await {
                Ok(Job::MonitorCreated(job)) => {
                    info!(
                        "Received MonitorCreated event for monitor {} (environment {}), checking immediately",
                        job.monitor_id, job.environment_id
                    );

                    let service = self.clone();
                    tokio::spawn(async move {
                        // Fetch the monitor and check it immediately
                        match status_monitors::Entity::find_by_id(job.monitor_id)
                            .one(service.db.as_ref())
                            .await
                        {
                            Ok(Some(monitor)) => {
                                if let Err(e) = Self::check_monitor(
                                    service.db.clone(),
                                    service.http_client.clone(),
                                    service.config_service.clone(),
                                    monitor,
                                    service.job_queue.clone(),
                                )
                                .await
                                {
                                    error!(
                                        "Failed to check newly created monitor {}: {:?}",
                                        job.monitor_id, e
                                    );
                                } else {
                                    info!(
                                        "Successfully checked newly created monitor {}",
                                        job.monitor_id
                                    );
                                }
                            }
                            Ok(None) => {
                                warn!(
                                    "Monitor {} not found after MonitorCreated event",
                                    job.monitor_id
                                );
                            }
                            Err(e) => {
                                error!("Failed to fetch monitor {}: {:?}", job.monitor_id, e);
                            }
                        }
                    });
                }
                Ok(_) => {
                    // Ignore other job types
                }
                Err(e) => {
                    error!("Error receiving job in health check scheduler: {:?}", e);
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
        }
    }

    /// Filter out monitors whose environment has on_demand enabled.
    /// Health checks go through the proxy and reset the idle timer,
    /// which would prevent scale-to-zero from ever triggering.
    fn filter_on_demand_monitors(
        monitors_with_envs: Vec<(status_monitors::Model, Option<environments::Model>)>,
    ) -> Vec<status_monitors::Model> {
        monitors_with_envs
            .into_iter()
            .filter(|(monitor, env)| {
                if let Some(env) = env {
                    let is_on_demand = env
                        .deployment_config
                        .as_ref()
                        .map(|dc| dc.on_demand)
                        .unwrap_or(false);
                    if is_on_demand {
                        debug!(
                            "Skipping monitor {} for on-demand environment {} ({})",
                            monitor.id, env.id, env.name
                        );
                        return false;
                    }
                }
                true
            })
            .map(|(monitor, _)| monitor)
            .collect()
    }

    /// Check a specific environment using its deployment URL
    pub async fn check_environment(
        &self,
        environment_id: i32,
    ) -> Result<(String, Option<i32>), StatusPageError> {
        // Get the environment
        let _environment = environments::Entity::find_by_id(environment_id)
            .one(self.db.as_ref())
            .await?
            .ok_or(StatusPageError::NotFound)?;

        // Get the latest deployment
        let deployment = deployments::Entity::find()
            .filter(deployments::Column::EnvironmentId.eq(environment_id))
            .filter(deployments::Column::State.eq("completed"))
            .one(self.db.as_ref())
            .await?;

        if deployment.is_none() {
            return Ok(("no_deployment".to_string(), None));
        }

        let deployment = deployment.unwrap();

        // Get the deployment container
        let container = deployment_containers::Entity::find()
            .filter(deployment_containers::Column::DeploymentId.eq(deployment.id))
            .one(self.db.as_ref())
            .await?;

        if container.is_none() {
            return Ok(("no_container".to_string(), None));
        }

        let container = container.unwrap();

        // Construct the check URL
        let check_url = format!(
            "http://{}:{}/",
            container.container_name, container.container_port
        );

        // Perform the check
        let start_time = std::time::Instant::now();
        let check_result = timeout(
            Duration::from_secs(10),
            self.http_client.get(&check_url).send(),
        )
        .await;

        let response_time_ms = start_time.elapsed().as_millis() as i32;

        match check_result {
            Ok(Ok(response)) => {
                let code = response.status();
                let status = if code.is_success() || code.as_u16() == 404 || code.as_u16() == 405 {
                    "operational"
                } else if code.is_server_error() {
                    "major_outage"
                } else {
                    "degraded"
                };
                Ok((status.to_string(), Some(response_time_ms)))
            }
            Ok(Err(_)) => Ok(("major_outage".to_string(), Some(response_time_ms))),
            Err(_) => Ok(("major_outage".to_string(), Some(10000))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use temps_entities::deployment_config::DeploymentConfig;
    use temps_entities::upstream_config::UpstreamList;

    fn make_monitor(id: i32, env_id: Option<i32>) -> status_monitors::Model {
        status_monitors::Model {
            id,
            project_id: 1,
            environment_id: env_id,
            name: format!("monitor-{}", id),
            monitor_type: "web".to_string(),
            check_path: None,
            check_interval_seconds: 60,
            is_active: true,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn make_env(id: i32, on_demand: bool) -> environments::Model {
        let deployment_config = if on_demand {
            Some(DeploymentConfig {
                on_demand: true,
                idle_timeout_seconds: 60,
                ..Default::default()
            })
        } else {
            Some(DeploymentConfig::default())
        };

        environments::Model {
            id,
            name: format!("env-{}", id),
            slug: format!("env-{}", id),
            subdomain: format!("proj-env-{}", id),
            last_deployment: None,
            host: String::new(),
            upstreams: UpstreamList::default(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            project_id: 1,
            current_deployment_id: Some(1),
            branch: None,
            deleted_at: None,
            deployment_config,
            is_preview: false,
            protected: false,
            sleeping: false,
            last_activity_at: None,
        }
    }

    #[test]
    fn test_filter_skips_on_demand_monitors() {
        let input = vec![
            (make_monitor(1, Some(10)), Some(make_env(10, true))),
            (make_monitor(2, Some(20)), Some(make_env(20, false))),
        ];

        let result = HealthCheckService::filter_on_demand_monitors(input);

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, 2);
    }

    #[test]
    fn test_filter_keeps_monitors_without_environment() {
        let input = vec![(make_monitor(1, None), None)];

        let result = HealthCheckService::filter_on_demand_monitors(input);

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, 1);
    }

    #[test]
    fn test_filter_keeps_normal_monitors() {
        let input = vec![
            (make_monitor(1, Some(10)), Some(make_env(10, false))),
            (make_monitor(2, Some(20)), Some(make_env(20, false))),
            (make_monitor(3, Some(30)), Some(make_env(30, false))),
        ];

        let result = HealthCheckService::filter_on_demand_monitors(input);

        assert_eq!(result.len(), 3);
    }

    #[test]
    fn test_filter_skips_all_on_demand() {
        let input = vec![
            (make_monitor(1, Some(10)), Some(make_env(10, true))),
            (make_monitor(2, Some(20)), Some(make_env(20, true))),
        ];

        let result = HealthCheckService::filter_on_demand_monitors(input);

        assert!(result.is_empty());
    }

    #[test]
    fn test_filter_keeps_monitor_with_no_deployment_config() {
        let mut env = make_env(10, false);
        env.deployment_config = None;

        let input = vec![(make_monitor(1, Some(10)), Some(env))];

        let result = HealthCheckService::filter_on_demand_monitors(input);

        assert_eq!(result.len(), 1);
    }

    #[test]
    fn test_filter_mixed_on_demand_and_normal() {
        let input = vec![
            (make_monitor(1, Some(10)), Some(make_env(10, true))), // on-demand -> skip
            (make_monitor(2, Some(20)), Some(make_env(20, false))), // normal -> keep
            (make_monitor(3, None), None),                         // no env -> keep
            (make_monitor(4, Some(40)), Some(make_env(40, true))), // on-demand -> skip
            (make_monitor(5, Some(50)), Some(make_env(50, false))), // normal -> keep
        ];

        let result = HealthCheckService::filter_on_demand_monitors(input);

        assert_eq!(result.len(), 3);
        assert_eq!(result[0].id, 2);
        assert_eq!(result[1].id, 3);
        assert_eq!(result[2].id, 5);
    }
}
