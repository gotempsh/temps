use chrono::Utc;
use sea_orm::{ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, Set};
use std::sync::Arc;
use std::time::Duration;
use temps_config::ConfigService;
use temps_core::{Job, JobReceiver};
use temps_entities::{
    deployment_containers, deployments, environments, projects, status_checks, status_monitors,
};
use tokio::time::{sleep, timeout};
use tracing::{debug, error, info, warn};

use super::types::StatusPageError;

/// Service for performing health checks on monitored environments
pub struct HealthCheckService {
    db: Arc<DatabaseConnection>,
    http_client: reqwest::Client,
    config_service: Arc<ConfigService>,
}

impl HealthCheckService {
    /// Create a new HealthCheckService with mandatory ConfigService
    pub fn new(db: Arc<DatabaseConnection>, config_service: Arc<ConfigService>) -> Self {
        let http_client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent("Temps-Status-Monitor/1.0")
            .build()
            .expect("Failed to create HTTP client");

        Self {
            db,
            http_client,
            config_service,
        }
    }

    /// Run health checks for all active monitors
    pub async fn run_all_checks(&self) -> Result<(), StatusPageError> {
        debug!("Starting health check cycle");

        // Get all active monitors
        let monitors = status_monitors::Entity::find()
            .filter(status_monitors::Column::IsActive.eq(true))
            .all(self.db.as_ref())
            .await?;

        debug!("Found {} active monitors to check", monitors.len());

        // Run checks concurrently with a limit
        let semaphore = Arc::new(tokio::sync::Semaphore::new(10)); // Limit concurrent checks
        let mut tasks = Vec::new();

        for monitor in monitors {
            let db = self.db.clone();
            let http_client = self.http_client.clone();
            let config_service = self.config_service.clone();
            let permit = semaphore.clone().acquire_owned().await.unwrap();

            let task = tokio::spawn(async move {
                let _permit = permit; // Hold permit until task completes
                if let Err(e) = Self::check_monitor(db, http_client, config_service, monitor).await
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
                // Append /health endpoint if monitor type is "health"
                if monitor.monitor_type == "health" {
                    format!("{}/health", public_url.trim_end_matches('/'))
                } else {
                    public_url
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

                    let status = if status_code.is_success() {
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
                    )
                    .await;
                }
                Ok(Err(e)) => {
                    // Network errors - retry for connection and timeout errors
                    if e.is_connect() || e.is_timeout() {
                        if attempt < MAX_RETRIES {
                            last_error = Some(format!(
                                "{}: {} (attempt {})",
                                if e.is_connect() {
                                    "Connection failed"
                                } else {
                                    "Request timeout"
                                },
                                e,
                                attempt + 1
                            ));
                            continue;
                        }
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
        )
        .await
    }

    /// Record a check result in the database with retry logic
    async fn record_check(
        db: &Arc<DatabaseConnection>,
        monitor_id: i32,
        status: String,
        response_time_ms: Option<i32>,
        error_message: Option<String>,
    ) -> Result<(), StatusPageError> {
        let check = status_checks::ActiveModel {
            monitor_id: Set(monitor_id),
            status: Set(status),
            response_time_ms: Set(response_time_ms),
            checked_at: Set(Utc::now()),
            error_message: Set(error_message),
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

        // Get all environments with their projects
        let environments_with_projects = environments::Entity::find()
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
                let status = if response.status().is_success() {
                    "operational"
                } else if response.status().is_server_error() {
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
