// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QuerySelect, Set,
    TransactionTrait,
};
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

/// Grace period before the scheduler runs its first health check cycle.
///
/// In split mode (`temps proxy` + `temps serve --role=console`) these are
/// independent OS processes with no startup handshake between them, and even
/// in single-process mode the proxy's listener setup (route/project-change
/// listeners, admin gate, DB connections) can still be in flight when the
/// console's plugins finish registering. A check that fires the instant this
/// scheduler starts races the proxy socket bind: `check_monitor` gets a
/// connection-refused, which is treated as a definitive "container is down"
/// and reported as `major_outage` with no retry (see the `is_connect()`
/// branch below). This delay absorbs that boot window so the first cycle
/// observes the platform in its normal running state; every cycle after the
/// first runs on the regular interval.
const STARTUP_GRACE_PERIOD: Duration = Duration::from_secs(20);

#[derive(Clone)]
struct MonitorProbeSnapshot {
    monitor_id: i32,
    deployment_id: i32,
    monitor_updated_at: temps_core::UtcDateTime,
}

/// Service for performing health checks on monitored environments
pub struct HealthCheckService {
    db: Arc<DatabaseConnection>,
    http_client: reqwest::Client,
    config_service: Arc<ConfigService>,
    job_queue: Arc<dyn JobQueue>,
}

impl HealthCheckService {
    /// Match deployment-readiness semantics: a redirect is a valid public
    /// application response (login/setup redirects are common), while 4xx/5xx
    /// indicate that the configured application URL is not usable.
    fn is_operational_http_status(status: reqwest::StatusCode) -> bool {
        status.is_success() || status.is_redirection()
    }

    /// Create a new HealthCheckService with mandatory ConfigService and JobQueue
    pub fn new(
        db: Arc<DatabaseConnection>,
        config_service: Arc<ConfigService>,
        job_queue: Arc<dyn JobQueue>,
    ) -> Result<Self, StatusPageError> {
        let http_client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent("Temps-Status-Monitor/1.0")
            // The application controls redirect targets. Following them would
            // turn periodic monitoring into a blind SSRF primitive against
            // loopback/private/link-local services, and would also prevent us
            // from classifying the original 3xx as a healthy app response.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|source| StatusPageError::HttpClientBuild { source })?;

        Ok(Self {
            db,
            http_client,
            config_service,
            job_queue,
        })
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
            let permit = match semaphore.clone().acquire_owned().await {
                Ok(permit) => permit,
                Err(error) => {
                    error!(
                        ?error,
                        "Health-check concurrency limiter closed unexpectedly"
                    );
                    break;
                }
            };

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

    /// Recompute all active monitors for one deployed environment.
    ///
    /// Deployment success calls this after updating the monitor's health path,
    /// avoiding up to a minute of stale Down/Unknown state while preserving the
    /// periodic scheduler's scale-to-zero exclusion for on-demand environments.
    pub async fn check_monitors_for_environment(
        &self,
        project_id: i32,
        environment_id: i32,
    ) -> Result<usize, StatusPageError> {
        let monitors_with_envs = status_monitors::Entity::find()
            .filter(status_monitors::Column::ProjectId.eq(project_id))
            .filter(status_monitors::Column::EnvironmentId.eq(Some(environment_id)))
            .filter(status_monitors::Column::IsActive.eq(true))
            .find_also_related(environments::Entity)
            .all(self.db.as_ref())
            .await?;
        let monitors = Self::filter_on_demand_monitors(monitors_with_envs);
        let monitor_count = monitors.len();

        for monitor in monitors {
            Self::check_monitor(
                self.db.clone(),
                self.http_client.clone(),
                self.config_service.clone(),
                monitor,
                self.job_queue.clone(),
            )
            .await?;
        }

        Ok(monitor_count)
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
        let Some(current_deployment_id) = environment.current_deployment_id else {
            warn!("Environment {} has no current deployment", env_id);
            return Ok(());
        };

        // Skip monitors whose deployment was intentionally paused by the
        // user. This is a live read (not a value cached earlier in the
        // caller) so it also covers the immediate check fired right after a
        // monitor is created, and it can't be stale relative to
        // `pause_deployment`, which persists `state = "paused"` before it
        // stops any containers. This is only the cheap early exit — it
        // avoids the HTTP round trip in the common case, but a pause can
        // still land *during* the request below (which may take several
        // seconds across retries), so `record_check` re-checks live again
        // right before persisting any outcome.
        if Self::is_deployment_paused(&db, current_deployment_id).await {
            debug!(
                "Skipping monitor {} for paused deployment {}",
                monitor.id, current_deployment_id
            );
            return Ok(());
        }
        let probe = MonitorProbeSnapshot {
            monitor_id: monitor.id,
            deployment_id: current_deployment_id,
            monitor_updated_at: monitor.updated_at,
        };

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
                    probe.clone(),
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

                    let status = if Self::is_operational_http_status(status_code) {
                        // Redirects are healthy (many apps redirect `/` to login or
                        // setup); client/server errors are not usable.
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
                        probe.clone(),
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
                        probe.clone(),
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
                        probe.clone(),
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
            probe,
            "major_outage".to_string(),
            Some(total_response_time_ms),
            Some(last_error.unwrap_or_else(|| "Unknown error after retries".to_string())),
            &job_queue,
        )
        .await
    }

    /// Check if a deployment is currently paused. Always a live read (never
    /// cached or captured earlier), since it feeds an alarm-suppression
    /// decision that must reflect `pause_deployment`'s state at the moment
    /// the caller is about to act, not at some earlier point in the check.
    async fn is_deployment_paused(db: &Arc<DatabaseConnection>, deployment_id: i32) -> bool {
        deployments::Entity::find_by_id(deployment_id)
            .one(db.as_ref())
            .await
            .ok()
            .flatten()
            .map(|d| d.state == "paused")
            .unwrap_or(false)
    }

    /// Record a check result in the database with retry logic and emit job for outage detection.
    ///
    /// Re-checks `deployment_id` for a pause live, right before persisting
    /// anything: `check_monitor`'s own paused guard only runs once, before
    /// the HTTP request, but that request (with retries) can take several
    /// seconds — long enough for a pause to land in between. Without this,
    /// an in-flight check that started against a running deployment could
    /// still record a "major_outage" for one that finished pausing seconds
    /// later.
    async fn record_check(
        db: &Arc<DatabaseConnection>,
        probe: MonitorProbeSnapshot,
        status: String,
        response_time_ms: Option<i32>,
        error_message: Option<String>,
        job_queue: &Arc<dyn JobQueue>,
    ) -> Result<(), StatusPageError> {
        let check = status_checks::ActiveModel {
            monitor_id: Set(probe.monitor_id),
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
                    probe.monitor_id, attempt, MAX_DB_RETRIES, delay
                );
                sleep(Duration::from_millis(delay)).await;
            }

            match Self::persist_check_if_current(
                db,
                probe.monitor_id,
                probe.deployment_id,
                probe.monitor_updated_at,
                check.clone(),
            )
            .await
            {
                Ok(false) => {
                    debug!(
                        monitor_id = probe.monitor_id,
                        deployment_id = probe.deployment_id,
                        "Discarded health result because its deployment is no longer current or was paused"
                    );
                    return Ok(());
                }
                Ok(true) => {
                    if attempt > 0 {
                        debug!("Database insert succeeded after {} attempts", attempt + 1);
                    }

                    // CRITICAL: Emit job for outage detection immediately after recording check
                    let job = Job::StatusCheckCompleted(StatusCheckCompletedJob {
                        monitor_id: probe.monitor_id,
                        status: status.clone(),
                        error_message: error_message.clone(),
                    });

                    if let Err(e) = job_queue.send(job).await {
                        error!(
                            "Failed to emit StatusCheckCompleted job for monitor {}: {:?}",
                            probe.monitor_id, e
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
                            probe.monitor_id,
                            attempt + 1,
                            e
                        );
                        last_error = Some(e);
                        continue;
                    }

                    // Non-retryable error or final attempt
                    error!(
                        "Failed to record check for monitor {} after {} attempts: {:?}",
                        probe.monitor_id,
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

    /// Commit a result only while the checked deployment is still the
    /// environment's current, unpaused deployment. The environment row lock
    /// gives deployment promotion and this insert one database ordering, and
    /// the monitor lock serializes concurrent probes for the same endpoint.
    async fn persist_check_if_current(
        db: &Arc<DatabaseConnection>,
        monitor_id: i32,
        deployment_id: i32,
        expected_monitor_updated_at: temps_core::UtcDateTime,
        check: status_checks::ActiveModel,
    ) -> Result<bool, sea_orm::DbErr> {
        let monitor_snapshot = status_monitors::Entity::find_by_id(monitor_id)
            .one(db.as_ref())
            .await?;
        let Some(environment_id) = monitor_snapshot.and_then(|monitor| monitor.environment_id)
        else {
            return Ok(false);
        };

        let transaction = db.begin().await?;
        let environment = environments::Entity::find_by_id(environment_id)
            .lock_exclusive()
            .one(&transaction)
            .await?;
        let Some(environment) = environment else {
            return Ok(false);
        };
        if environment.current_deployment_id != Some(deployment_id) {
            return Ok(false);
        }

        let deployment = deployments::Entity::find_by_id(deployment_id)
            .lock_exclusive()
            .one(&transaction)
            .await?;
        if deployment
            .as_ref()
            .is_none_or(|model| model.state == "paused")
        {
            return Ok(false);
        }

        let monitor = status_monitors::Entity::find_by_id(monitor_id)
            .lock_exclusive()
            .one(&transaction)
            .await?;
        if monitor.as_ref().and_then(|model| model.environment_id) != Some(environment_id)
            || monitor.as_ref().is_none_or(|model| {
                !model.is_active || model.updated_at != expected_monitor_updated_at
            })
        {
            return Ok(false);
        }

        check.insert(&transaction).await?;
        transaction.commit().await?;
        Ok(true)
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

        // See STARTUP_GRACE_PERIOD: wait out the proxy's boot window before
        // touching any monitor. This also delays `initialize_monitors` below,
        // which prevents the MonitorCreated events it emits for newly-seen
        // environments from triggering an immediate check during the same
        // race window.
        sleep(STARTUP_GRACE_PERIOD).await;

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
                let status = if Self::is_operational_http_status(code) {
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
            is_managed: false,
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
            attack_mode: None,
            force_https: None,
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

    #[test]
    fn test_operational_http_status_matches_deployment_readiness() {
        for status in [200, 204, 301, 302, 307, 308] {
            let status = reqwest::StatusCode::from_u16(status).unwrap();
            assert!(
                HealthCheckService::is_operational_http_status(status),
                "HTTP {status} should be operational"
            );
        }

        for status in [400, 401, 403, 404, 405, 429, 500, 502, 503] {
            let status = reqwest::StatusCode::from_u16(status).unwrap();
            assert!(
                !HealthCheckService::is_operational_http_status(status),
                "HTTP {status} should not be operational"
            );
        }
    }

    // ── check_monitor: paused-deployment skip ──────────────────────────
    //
    // Regression coverage for the gap Greptile flagged on PR #835: the
    // paused check used to live only in `run_all_checks`'s pre-filter, so
    // the immediate check fired by the `MonitorCreated` job (which calls
    // `check_monitor` directly, bypassing that pre-filter) still reported a
    // freshly-paused deployment as down. The check now lives inside
    // `check_monitor` itself — the single place every caller goes through —
    // and reads deployment state live rather than from a value the caller
    // captured earlier, so it can't be stale relative to
    // `pause_deployment`, which persists `state = "paused"` before it stops
    // any containers.

    struct NeverJobQueue;
    #[async_trait::async_trait]
    impl temps_core::JobQueue for NeverJobQueue {
        async fn send(&self, _job: temps_core::Job) -> Result<(), temps_core::QueueError> {
            Ok(())
        }
        fn subscribe(&self) -> Box<dyn temps_core::JobReceiver> {
            struct NeverReceiver;
            #[async_trait::async_trait]
            impl temps_core::JobReceiver for NeverReceiver {
                async fn recv(&mut self) -> Result<temps_core::Job, temps_core::QueueError> {
                    std::future::pending().await
                }
            }
            Box::new(NeverReceiver)
        }
    }

    fn test_config_service(
        db: &Arc<DatabaseConnection>,
        database_url: &str,
    ) -> Arc<temps_config::ConfigService> {
        let config = temps_config::ServerConfig::new(
            "127.0.0.1:3000".to_string(),
            database_url.to_string(),
            None,
            None,
        )
        .expect("failed to build test ServerConfig");
        Arc::new(temps_config::ConfigService::new(
            Arc::new(config),
            db.clone(),
        ))
    }

    #[tokio::test]
    async fn test_environment_recheck_selects_monitor_and_skips_paused_deployment() {
        let Ok(test_db) = temps_database::test_utils::TestDatabase::with_migrations().await else {
            println!("Docker not available, skipping");
            return;
        };
        let db = test_db.connection_arc();

        let project = temps_entities::projects::ActiveModel {
            name: Set("Paused Skip Test".to_string()),
            repo_name: Set("test-repo".to_string()),
            repo_owner: Set("test-owner".to_string()),
            slug: Set("paused-skip-test".to_string()),
            preset: Set(temps_entities::preset::Preset::NextJs),
            directory: Set("/test".to_string()),
            main_branch: Set("main".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .unwrap();

        let environment = environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("production".to_string()),
            slug: Set("production".to_string()),
            subdomain: Set("paused-skip-test-production".to_string()),
            host: Set("paused-skip-test-production.test.local".to_string()),
            upstreams: Set(UpstreamList::default()),
            branch: Set(Some("main".to_string())),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .unwrap();

        let deployment = deployments::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            slug: Set("paused-skip-test-1".to_string()),
            state: Set("paused".to_string()),
            metadata: Set(Some(deployments::DeploymentMetadata::default())),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .unwrap();

        let mut active_environment: environments::ActiveModel = environment.clone().into();
        active_environment.current_deployment_id = Set(Some(deployment.id));
        let environment = active_environment.update(db.as_ref()).await.unwrap();

        let monitor = status_monitors::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(Some(environment.id)),
            name: Set("production health".to_string()),
            monitor_type: Set("web".to_string()),
            check_interval_seconds: Set(60),
            is_active: Set(true),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .unwrap();

        let config_service = test_config_service(&db, &test_db.database_url);
        let job_queue: Arc<dyn temps_core::JobQueue> = Arc::new(NeverJobQueue);
        let health_check_service = HealthCheckService::new(db.clone(), config_service, job_queue)
            .expect("test HTTP client should build");

        // If the paused check were skipped or stale, this would try to hit
        // `paused-skip-test-production.test.local`, which doesn't resolve —
        // the call would still return `Ok(())` (check_monitor treats a
        // failed request as a recorded outage, not an error) but it would
        // insert a `status_checks` row. Assert none was written instead of
        // asserting on the return value, so the test fails loudly if the
        // guard regresses instead of passing for the wrong reason.
        let checked = health_check_service
            .check_monitors_for_environment(project.id, environment.id)
            .await
            .expect("post-deploy environment recheck should succeed");
        assert_eq!(checked, 1, "the environment's active monitor is selected");

        let checks = status_checks::Entity::find()
            .filter(status_checks::Column::MonitorId.eq(monitor.id))
            .all(db.as_ref())
            .await
            .unwrap();
        assert!(
            checks.is_empty(),
            "check_monitor must not perform an HTTP check against a paused deployment"
        );
    }

    /// Regression test for the mid-check race Greptile flagged: `check_monitor`'s
    /// own paused guard only runs once, before the HTTP request — but that
    /// request (with up to 3 retries) can take several seconds, long enough
    /// for a pause to land in between. `record_check` must re-check live,
    /// right before persisting any outcome, regardless of what the caller
    /// observed earlier. Exercise `record_check` directly (rather than the
    /// full `check_monitor`, whose early guard would trivially skip a
    /// deployment that's already paused at entry) so this only passes if the
    /// *late* guard inside `record_check` itself is the one doing the work.
    #[tokio::test]
    async fn test_record_check_skips_paused_deployment_mid_check() {
        let Ok(test_db) = temps_database::test_utils::TestDatabase::with_migrations().await else {
            println!("Docker not available, skipping");
            return;
        };
        let db = test_db.connection_arc();

        let project = temps_entities::projects::ActiveModel {
            name: Set("Mid-Check Pause Test".to_string()),
            repo_name: Set("test-repo".to_string()),
            repo_owner: Set("test-owner".to_string()),
            slug: Set("mid-check-pause-test".to_string()),
            preset: Set(temps_entities::preset::Preset::NextJs),
            directory: Set("/test".to_string()),
            main_branch: Set("main".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .unwrap();

        let environment = environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("production".to_string()),
            slug: Set("production".to_string()),
            subdomain: Set("mid-check-pause-test-production".to_string()),
            host: Set("mid-check-pause-test-production.test.local".to_string()),
            upstreams: Set(UpstreamList::default()),
            branch: Set(Some("main".to_string())),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .unwrap();

        // Deployment starts NOT paused — modeling "check_monitor's early
        // guard saw a running deployment and proceeded to the HTTP request".
        // It's paused (below) only after that point, simulating the pause
        // landing while the check is in flight.
        let deployment = deployments::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            slug: Set("mid-check-pause-test-1".to_string()),
            state: Set("deployed".to_string()),
            metadata: Set(Some(deployments::DeploymentMetadata::default())),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .unwrap();

        let mut active_environment: environments::ActiveModel = environment.clone().into();
        active_environment.current_deployment_id = Set(Some(deployment.id));
        active_environment.update(db.as_ref()).await.unwrap();

        let monitor = status_monitors::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(Some(environment.id)),
            name: Set("production health".to_string()),
            monitor_type: Set("web".to_string()),
            check_interval_seconds: Set(60),
            is_active: Set(true),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .unwrap();

        // Pause lands mid-check, after the (simulated) early guard already
        // passed.
        let mut active_deployment: deployments::ActiveModel = deployment.clone().into();
        active_deployment.state = Set("paused".to_string());
        active_deployment.update(db.as_ref()).await.unwrap();

        let job_queue: Arc<dyn temps_core::JobQueue> = Arc::new(NeverJobQueue);

        let result = HealthCheckService::record_check(
            &db,
            MonitorProbeSnapshot {
                monitor_id: monitor.id,
                deployment_id: deployment.id,
                monitor_updated_at: monitor.updated_at,
            },
            "major_outage".to_string(),
            Some(5000),
            Some("Connection failed".to_string()),
            &job_queue,
        )
        .await;
        assert!(result.is_ok());

        let checks = status_checks::Entity::find()
            .filter(status_checks::Column::MonitorId.eq(monitor.id))
            .all(db.as_ref())
            .await
            .unwrap();
        assert!(
            checks.is_empty(),
            "record_check must not persist a check result for a deployment paused mid-check, \
             even when the caller's own paused guard already passed"
        );
    }

    #[tokio::test]
    async fn test_record_check_discards_result_from_replaced_deployment() {
        let Ok(test_db) = temps_database::test_utils::TestDatabase::with_migrations().await else {
            println!("Docker not available, skipping");
            return;
        };
        let db = test_db.connection_arc();
        let project = temps_entities::projects::ActiveModel {
            name: Set("Deployment Replacement Test".to_string()),
            repo_name: Set("test-repo".to_string()),
            repo_owner: Set("test-owner".to_string()),
            slug: Set("deployment-replacement-test".to_string()),
            preset: Set(temps_entities::preset::Preset::NextJs),
            directory: Set("/test".to_string()),
            main_branch: Set("main".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .unwrap();
        let environment = environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("production".to_string()),
            slug: Set("production".to_string()),
            subdomain: Set("deployment-replacement-test-production".to_string()),
            host: Set("deployment-replacement-test-production.test.local".to_string()),
            upstreams: Set(UpstreamList::default()),
            branch: Set(Some("main".to_string())),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .unwrap();
        let old_deployment = deployments::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            slug: Set("replacement-old".to_string()),
            state: Set("deployed".to_string()),
            metadata: Set(Some(deployments::DeploymentMetadata::default())),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .unwrap();
        let current_deployment = deployments::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            slug: Set("replacement-current".to_string()),
            state: Set("deployed".to_string()),
            metadata: Set(Some(deployments::DeploymentMetadata::default())),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .unwrap();
        let environment_id = environment.id;
        let mut active_environment: environments::ActiveModel = environment.into();
        active_environment.current_deployment_id = Set(Some(current_deployment.id));
        active_environment.update(db.as_ref()).await.unwrap();
        let monitor = status_monitors::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(Some(environment_id)),
            name: Set("production health".to_string()),
            monitor_type: Set("web".to_string()),
            check_interval_seconds: Set(60),
            is_active: Set(true),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .unwrap();
        let job_queue: Arc<dyn temps_core::JobQueue> = Arc::new(NeverJobQueue);

        HealthCheckService::record_check(
            &db,
            MonitorProbeSnapshot {
                monitor_id: monitor.id,
                deployment_id: old_deployment.id,
                monitor_updated_at: monitor.updated_at,
            },
            "major_outage".to_string(),
            Some(5000),
            Some("stale failure".to_string()),
            &job_queue,
        )
        .await
        .unwrap();

        let checks = status_checks::Entity::find()
            .filter(status_checks::Column::MonitorId.eq(monitor.id))
            .all(db.as_ref())
            .await
            .unwrap();
        assert!(
            checks.is_empty(),
            "a result started for an older deployment must not become current status"
        );

        let probed_monitor_updated_at = monitor.updated_at;
        let mut changed_monitor: status_monitors::ActiveModel = monitor.clone().into();
        changed_monitor.check_path = Set(Some("/ready".to_string()));
        changed_monitor.update(db.as_ref()).await.unwrap();
        HealthCheckService::record_check(
            &db,
            MonitorProbeSnapshot {
                monitor_id: monitor.id,
                deployment_id: current_deployment.id,
                monitor_updated_at: probed_monitor_updated_at,
            },
            "major_outage".to_string(),
            Some(5000),
            Some("result from the old path".to_string()),
            &job_queue,
        )
        .await
        .unwrap();
        let checks = status_checks::Entity::find()
            .filter(status_checks::Column::MonitorId.eq(monitor.id))
            .all(db.as_ref())
            .await
            .unwrap();
        assert!(
            checks.is_empty(),
            "a result from a monitor path changed mid-probe must be discarded"
        );
    }
}
