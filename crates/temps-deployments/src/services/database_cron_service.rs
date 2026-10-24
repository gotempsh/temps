// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Database Cron Configuration Service
//!
//! Implements cron job configuration using the database

use async_trait::async_trait;
use chrono::{DateTime, TimeZone as _, Timelike as _, Utc};
use cron::Schedule;
use sea_orm::sea_query::Expr;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, DatabaseConnection, EntityTrait, PaginatorTrait,
    QueryFilter, QueryOrder, Select, Set,
};
use std::collections::HashSet;
use std::str::FromStr;
use std::sync::{Arc, OnceLock};
use temps_core::UtcDateTime;
use temps_database::DbConnection;
use temps_entities::{cron_executions, crons, deployment_containers, deployments};
use temps_monitoring::alarm_service::{AlarmService, AlarmSeverity, AlarmType, FireAlarmRequest};
use thiserror::Error;
use tokio::time::{self, Duration};
use tracing::{debug, error, info, warn};

use crate::jobs::configure_crons::{CronConfig, CronConfigError, CronConfigService};
use crate::services::cron_jobs::{
    deployment_container_import, insert_running, job_dispatch_decision, job_spec, run_name,
    CronJobColumns, JobDispatch, CRON_OWNER_KIND, RECORD_RUN_FINALIZER,
};
use crate::services::deployment_token_service::DeploymentTokenService;
use temps_core::CronConcurrencyPolicy;
use temps_entities::{environments, projects};
use temps_workload_plane::{ApplyRequest, Owner, WorkloadPlane};

#[derive(Error, Debug)]
pub enum CronServiceError {
    #[error("Database error: {0}")]
    DatabaseError(#[from] sea_orm::DbErr),

    #[error("Invalid cron schedule '{schedule}': {message}")]
    InvalidSchedule { schedule: String, message: String },

    #[error("Maximum number of crons ({max}) reached for environment {env_id}")]
    MaxCronsReached { max: i64, env_id: i32 },

    #[error("Couldn't execute cron {cron_id} at URL {url}: {message}")]
    ExecutionError {
        cron_id: i32,
        url: String,
        message: String,
    },

    #[error("Failed to execute cron {cron_id} at URL {url}: {message}")]
    ExecutionFailed {
        cron_id: i32,
        url: String,
        /// HTTP status the endpoint answered with.
        status: u16,
        message: String,
    },

    #[error("No active deployment found for environment {env_id}")]
    NoActiveDeployment { env_id: i32 },

    #[error("HTTP request failed: {0}")]
    HttpError(#[from] reqwest::Error),

    #[error("Cron {cron_id} not found in project {project_id}, environment {env_id}")]
    CronNotFound {
        cron_id: i32,
        project_id: i32,
        env_id: i32,
    },

    #[error("Failed to get database connection: {0}")]
    ConnectionError(String),

    #[error("Failed to notify about cron error: {0}")]
    NotificationError(String),

    #[error("Workload plane {operation} for job cron {cron_id} failed: {message}")]
    WorkloadPlane {
        cron_id: i32,
        operation: &'static str,
        message: String,
    },

    #[error(
        "Job cron {cron_id} cannot run: deployment {deployment_id} of environment {env_id} \
         has no container image (a job runs in the environment's current deployment image)"
    )]
    NoJobImage {
        cron_id: i32,
        deployment_id: i32,
        env_id: i32,
    },
}

/// Floor for the cron scheduler's between-tick sleep.
///
/// See [`DatabaseCronConfigService::duration_until_next_minute`].
const MIN_SCHEDULER_SLEEP: Duration = Duration::from_secs(1);

/// Upper bound on raising and delivering one cron failure alarm. Delivery runs
/// off the scheduler, so this only bounds how long a stalled notification
/// provider can keep the task alive.
const ALARM_DELIVERY_TIMEOUT: Duration = Duration::from_secs(60);

/// Database-backed cron configuration service
pub struct DatabaseCronConfigService {
    db: Arc<DatabaseConnection>,
    http_client: Arc<reqwest::Client>,
    queue: Arc<dyn temps_core::JobQueue>,
    deployment_token_service: Arc<DeploymentTokenService>,
    /// Raises an alarm (and its notifications) when an invocation fails.
    /// Late-bound: the monitoring plugin registers after this one, so it is
    /// set from `initialize_plugin_services`. Unset means failures are only
    /// recorded in the execution history.
    alarm_service: OnceLock<Arc<AlarmService>>,
    /// Runs `job` crons (ADR-055 §14). Late-bound like the alarm service:
    /// the plane needs the audit logger, which is only resolvable once
    /// every plugin has registered. Unset means `job` crons are recorded
    /// as failed with that reason; `http` crons never use it.
    plane: OnceLock<Arc<WorkloadPlane>>,
}

/// Where a `job` cron's run comes from: the environment's current
/// deployment.
struct JobTarget {
    image: String,
    container_id: String,
    /// The node `container_id` runs on (`None`: the control plane). The run
    /// is pinned there: its variables are resolved for that node.
    container_node_id: Option<i32>,
    project_slug: String,
}

impl DatabaseCronConfigService {
    pub fn new(
        db: Arc<DbConnection>,
        queue: Arc<dyn temps_core::JobQueue>,
        deployment_token_service: Arc<DeploymentTokenService>,
    ) -> Self {
        Self {
            db,
            http_client: Arc::new(reqwest::Client::new()),
            queue,
            deployment_token_service,
            alarm_service: OnceLock::new(),
            plane: OnceLock::new(),
        }
    }

    /// Wire in the workload plane that runs `job` crons.
    pub fn set_workload_plane(&self, plane: Arc<WorkloadPlane>) {
        let _ = self.plane.set(plane);
    }

    /// Wire in the alarm service once every plugin has registered.
    pub fn set_alarm_service(&self, alarm_service: Arc<AlarmService>) {
        let _ = self.alarm_service.set(alarm_service);
    }

    /// Normalize a cron schedule to the 6-field format required by the `cron` crate.
    ///
    /// Users write standard 5-field cron expressions in `.temps.yaml`:
    ///   "minute hour day month weekday"  (e.g. "0 0 * * *")
    ///
    /// The `cron` crate expects 6 fields (seconds prepended):
    ///   "second minute hour day month weekday"  (e.g. "0 0 0 * * *")
    ///
    /// This function detects 5-field expressions and prepends "0 " for seconds.
    /// 6-field and 7-field expressions are passed through unchanged.
    fn normalize_schedule(schedule: &str) -> String {
        let fields: Vec<&str> = schedule.split_whitespace().collect();
        if fields.len() == 5 {
            format!("0 {}", schedule)
        } else {
            schedule.to_string()
        }
    }

    /// Validate a cron schedule expression
    fn validate_cron_schedule(schedule: &str) -> Result<(), CronConfigError> {
        let normalized = Self::normalize_schedule(schedule);

        // Parse the cron schedule
        let parsed_schedule = cron::Schedule::from_str(&normalized).map_err(|e| {
            CronConfigError::InvalidSchedule(format!(
                "Invalid cron expression '{}': {}",
                schedule, e
            ))
        })?;

        // Validate minimum interval (1 minute)
        let upcoming = parsed_schedule.upcoming(Utc);
        let next_two: Vec<_> = upcoming.take(2).collect();

        if let [first, second] = next_two.as_slice() {
            let duration = *second - *first;
            if duration.num_seconds() < 60 {
                return Err(CronConfigError::InvalidSchedule(
                    "Cron schedule must be at least 1 minute apart".to_string(),
                ));
            }
        }

        Ok(())
    }

    /// Calculate the next run time for a cron schedule
    fn calculate_next_run(schedule: &str) -> Result<UtcDateTime, CronConfigError> {
        let normalized = Self::normalize_schedule(schedule);

        let parsed_schedule = cron::Schedule::from_str(&normalized).map_err(|e| {
            CronConfigError::InvalidSchedule(format!("Invalid cron expression: {}", e))
        })?;

        let next_run = parsed_schedule.upcoming(Utc).next().ok_or_else(|| {
            CronConfigError::InvalidSchedule("No upcoming execution time".to_string())
        })?;

        Ok(next_run)
    }
}

#[async_trait]
impl CronConfigService for DatabaseCronConfigService {
    async fn configure_crons(
        &self,
        project_id: i32,
        environment_id: i32,
        cron_configs: Vec<CronConfig>,
    ) -> Result<(), CronConfigError> {
        info!(
            "Configuring {} cron job(s) for project {} in environment {}",
            cron_configs.len(),
            project_id,
            environment_id
        );

        // Validate all cron schedules first
        for cron in &cron_configs {
            Self::validate_cron_schedule(&cron.schedule)?;
        }

        // Fetch existing crons for this project/environment
        let existing_crons = crons::Entity::find()
            .filter(crons::Column::ProjectId.eq(project_id))
            .filter(crons::Column::EnvironmentId.eq(environment_id))
            .filter(crons::Column::DeletedAt.is_null())
            .all(self.db.as_ref())
            .await
            .map_err(|e| {
                CronConfigError::DatabaseError(format!("Failed to fetch existing crons: {}", e))
            })?;

        // Check max crons limit per environment
        const MAX_CRONS_PER_ENV: usize = 10;
        let existing_paths: HashSet<_> = existing_crons.iter().map(|c| c.path.as_str()).collect();
        let new_crons_count = cron_configs
            .iter()
            .filter(|c| !existing_paths.contains(c.path.as_str()))
            .count();

        if existing_crons.len() + new_crons_count > MAX_CRONS_PER_ENV {
            return Err(CronConfigError::ConfigError(format!(
                "Maximum number of crons ({}) would be exceeded. Current: {}, New: {}",
                MAX_CRONS_PER_ENV,
                existing_crons.len(),
                new_crons_count
            )));
        }

        // Create a set of paths from repo crons for easier comparison
        let repo_cron_paths: HashSet<_> = cron_configs.iter().map(|c| c.path.as_str()).collect();

        // Mark crons as deleted if they're not in the repo config
        for existing_cron in &existing_crons {
            if !repo_cron_paths.contains(existing_cron.path.as_str()) {
                info!(
                    "Marking cron job '{}' as deleted (no longer in .temps.yaml)",
                    existing_cron.path
                );

                let mut cron_update: crons::ActiveModel = existing_cron.clone().into();
                cron_update.deleted_at = Set(Some(Utc::now()));
                cron_update.update(self.db.as_ref()).await.map_err(|e| {
                    CronConfigError::DatabaseError(format!("Failed to delete cron: {}", e))
                })?;
            }
        }

        // Process each cron from the repo
        for cron_config in cron_configs {
            // Check if cron already exists
            let existing_cron = existing_crons.iter().find(|c| c.path == cron_config.path);
            let job_columns = CronJobColumns::from_config(&cron_config);

            match existing_cron {
                Some(cron) if cron.schedule != cron_config.schedule => {
                    // Update schedule if it changed
                    info!(
                        "Updating cron job '{}': schedule changed from '{}' to '{}'",
                        cron.path, cron.schedule, cron_config.schedule
                    );

                    let next_run = Self::calculate_next_run(&cron_config.schedule)?;

                    let mut cron_update: crons::ActiveModel = cron.clone().into();
                    cron_update.schedule = Set(cron_config.schedule.clone());
                    cron_update.updated_at = Set(Utc::now());
                    cron_update.next_run = Set(Some(next_run));
                    job_columns.apply(&mut cron_update);
                    cron_update.update(self.db.as_ref()).await.map_err(|e| {
                        CronConfigError::DatabaseError(format!("Failed to update cron: {}", e))
                    })?;

                    info!(
                        "Updated cron job '{}' with schedule '{}', next run at {:?}",
                        cron.path, cron_config.schedule, next_run
                    );
                }
                // Same schedule, changed job settings (command, policy,
                // limits): update them without moving the next run. Never
                // true for an `http` cron, whose columns stay at their
                // defaults.
                Some(cron) if !job_columns.matches(cron) => {
                    info!(
                        "Updating job cron '{}' (cron {}): run settings changed",
                        cron.path, cron.id
                    );
                    let mut cron_update: crons::ActiveModel = cron.clone().into();
                    cron_update.updated_at = Set(Utc::now());
                    job_columns.apply(&mut cron_update);
                    cron_update.update(self.db.as_ref()).await.map_err(|e| {
                        CronConfigError::DatabaseError(format!(
                            "Failed to update job cron {} ('{}'): {}",
                            cron.id, cron.path, e
                        ))
                    })?;
                }
                Some(cron) => {
                    info!(
                        "✓ Cron job '{}' already exists with same schedule",
                        cron.path
                    );
                }
                None => {
                    // Create new cron
                    info!("Creating new cron job '{}'", cron_config.path);

                    let next_run = Self::calculate_next_run(&cron_config.schedule)?;
                    let now = Utc::now();

                    let mut new_cron = crons::ActiveModel {
                        project_id: Set(project_id),
                        environment_id: Set(environment_id),
                        path: Set(cron_config.path.clone()),
                        schedule: Set(cron_config.schedule.clone()),
                        created_at: Set(now),
                        updated_at: Set(now),
                        next_run: Set(Some(next_run)),
                        deleted_at: Set(None),
                        ..Default::default()
                    };
                    job_columns.apply(&mut new_cron);

                    match new_cron.insert(self.db.as_ref()).await {
                        Ok(cron) => {
                            info!(
                                "Created cron job '{}' with schedule '{}', next run at {:?}",
                                cron.path, cron.schedule, cron.next_run
                            );
                        }
                        Err(e) => {
                            error!("Failed to create cron job '{}': {}", cron_config.path, e);
                            return Err(CronConfigError::DatabaseError(format!(
                                "Failed to create cron: {}",
                                e
                            )));
                        }
                    }
                }
            }
        }

        info!(
            "Successfully configured cron jobs for project {} in environment {}",
            project_id, environment_id
        );

        Ok(())
    }
}

impl DatabaseCronConfigService {
    /// Get all cron jobs for a specific environment
    pub async fn get_environment_crons(
        &self,
        project_id: i32,
        env_id: i32,
    ) -> Result<Vec<crons::Model>, CronConfigError> {
        let crons_list = crons::Entity::find()
            .filter(crons::Column::ProjectId.eq(project_id))
            .filter(crons::Column::EnvironmentId.eq(env_id))
            .filter(crons::Column::DeletedAt.is_null())
            .all(self.db.as_ref())
            .await
            .map_err(|e| CronConfigError::DatabaseError(format!("Failed to fetch crons: {}", e)))?;

        Ok(crons_list)
    }

    /// Get a specific cron job by ID
    pub async fn get_cron_by_id(
        &self,
        project_id: i32,
        env_id: i32,
        cron_id: i32,
    ) -> Result<crons::Model, CronConfigError> {
        crons::Entity::find_by_id(cron_id)
            .filter(crons::Column::ProjectId.eq(project_id))
            .filter(crons::Column::EnvironmentId.eq(env_id))
            .filter(crons::Column::DeletedAt.is_null())
            .one(self.db.as_ref())
            .await
            .map_err(|e| CronConfigError::DatabaseError(format!("Failed to fetch cron: {}", e)))?
            .ok_or_else(|| {
                CronConfigError::ConfigError(format!(
                    "Cron {} not found in project {}, environment {}",
                    cron_id, project_id, env_id
                ))
            })
    }

    /// Get execution history for a cron job
    pub async fn get_cron_executions(
        &self,
        project_id: i32,
        env_id: i32,
        cron_id: i32,
        page: i64,
        per_page: i64,
    ) -> Result<Vec<temps_entities::cron_executions::Model>, CronConfigError> {
        use temps_entities::cron_executions;

        // First verify the cron exists and belongs to the project/environment
        self.get_cron_by_id(project_id, env_id, cron_id).await?;

        let query = cron_executions::Entity::find()
            .filter(cron_executions::Column::CronId.eq(cron_id))
            .order_by_desc(cron_executions::Column::ExecutedAt);

        let paginator = query.paginate(self.db.as_ref(), per_page as u64);
        let executions = paginator.fetch_page((page - 1) as u64).await.map_err(|e| {
            CronConfigError::DatabaseError(format!("Failed to fetch executions: {}", e))
        })?;

        Ok(executions)
    }

    /// Time remaining until the top of the next minute.
    ///
    /// Replaces an `unwrap()`-based computation: `second()` and `nanosecond()`
    /// can exceed their usual range during a leap second, which made the old
    /// arithmetic capable of producing a negative `chrono::Duration` and
    /// panicking in `to_std()`. Clamping keeps the sleep inside one minute.
    ///
    /// Floored at [`MIN_SCHEDULER_SLEEP`]: during a leap second the clamp
    /// above can land a nanosecond short of the minute boundary, and a 1ns
    /// sleep would spin the scheduler loop — re-running the due query
    /// thousands of times in that second. One second of extra latency on a
    /// per-minute scheduler is not observable; a hot loop is.
    fn duration_until_next_minute(now: DateTime<Utc>) -> Duration {
        const NANOS_PER_MINUTE: u64 = 60 * 1_000_000_000;
        let into_minute = (u64::from(now.second()) * 1_000_000_000 + u64::from(now.nanosecond()))
            .min(NANOS_PER_MINUTE - 1);
        Duration::from_nanos(NANOS_PER_MINUTE - into_minute).max(MIN_SCHEDULER_SLEEP)
    }

    /// Select the crons that are due at `now`.
    ///
    /// The scheduler used to load the entire `crons` table every single
    /// minute and filter in Rust — 1,440 full scans a day, almost all of
    /// which decide to do nothing. `next_run` already existed and was already
    /// maintained; it just was not being used to narrow the query.
    ///
    /// `next_run IS NULL` is included so a row that has never been scheduled
    /// (or predates `next_run` being maintained) still gets picked up and
    /// seeded instead of sitting invisible forever. Soft-deleted rows are
    /// excluded here rather than in `process_cron` so they stop being fetched
    /// at all.
    fn due_crons_query(now: UtcDateTime) -> Select<crons::Entity> {
        crons::Entity::find()
            .filter(crons::Column::DeletedAt.is_null())
            .filter(
                Condition::any()
                    .add(crons::Column::NextRun.is_null())
                    .add(crons::Column::NextRun.lte(now)),
            )
    }

    /// Persist a cron's next occurrence.
    ///
    /// Uses `update_many` + `col_expr` rather than an ActiveModel save so the
    /// scheduler does not bump `updated_at` on every run — that column
    /// records when the *configuration* last changed, and the user sees it.
    async fn persist_next_run(
        &self,
        cron_id: i32,
        next_run: UtcDateTime,
    ) -> Result<(), CronServiceError> {
        crons::Entity::update_many()
            .col_expr(crons::Column::NextRun, Expr::value(next_run))
            .filter(crons::Column::Id.eq(cron_id))
            .exec(self.db.as_ref())
            .await?;
        Ok(())
    }

    /// Terminal `next_run` for a cron that can never fire again.
    ///
    /// The `crons` table has no `enabled` column, so `next_run` is the only
    /// place a "this will never run again" state can be recorded. `NULL` is
    /// already taken — [`Self::due_crons_query`] reads it as "never scheduled,
    /// seed me" — and any realistic timestamp is eventually reached by the
    /// `next_run <= now` predicate, so the sentinel is a fixed far-future
    /// instant. A retired cron is simply never selected by the due query
    /// again; the row stays visible, listable and editable, and re-deploying
    /// with a live schedule overwrites the sentinel through
    /// `configure_crons`'s normal `calculate_next_run` path.
    fn retired_sentinel() -> UtcDateTime {
        Utc.with_ymd_and_hms(9999, 12, 31, 23, 59, 59)
            .single()
            .unwrap_or(DateTime::<Utc>::MAX_UTC)
    }

    /// Park a cron whose schedule has no further occurrences on the terminal
    /// sentinel so the scheduler stops re-selecting it every minute.
    async fn retire_cron(&self, cron: &crons::Model) -> Result<(), CronServiceError> {
        info!(
            cron_id = cron.id,
            schedule = %cron.schedule,
            "Cron schedule has no further occurrences; retiring it (redeploy with a live schedule to resume)"
        );
        self.persist_next_run(cron.id, Self::retired_sentinel())
            .await
    }

    pub async fn start_cron_scheduler(&self) {
        debug!("Starting cron scheduler");

        loop {
            time::sleep(Self::duration_until_next_minute(Utc::now())).await;

            let now = Utc::now();

            // Use a block to ensure db connection is dropped after use
            let crons_list = match Self::due_crons_query(now).all(self.db.as_ref()).await {
                Ok(crons_list) => crons_list,
                Err(e) => {
                    error!("Failed to fetch due crons: {}", e);
                    continue;
                }
            };

            if crons_list.is_empty() {
                continue;
            }

            debug!("Processing {} due cron(s)", crons_list.len());

            // Process crons in chunks to limit concurrency
            for chunk in crons_list.chunks(10) {
                let futures: Vec<_> = chunk
                    .iter()
                    .map(|cron| self.process_cron(cron, now))
                    .collect();

                let results = futures::future::join_all(futures).await;
                for result in results {
                    if let Err(e) = result {
                        error!("Error processing cron: {}", e);
                    }
                }
            }
        }
    }

    async fn process_cron(
        &self,
        cron: &crons::Model,
        now: DateTime<Utc>,
    ) -> Result<(), CronServiceError> {
        // Skip if cron is deleted
        if cron.deleted_at.is_some() {
            return Ok(());
        }

        let normalized = Self::normalize_schedule(&cron.schedule);
        let schedule =
            Schedule::from_str(&normalized).map_err(|e| CronServiceError::InvalidSchedule {
                schedule: cron.schedule.clone(),
                message: e.to_string(),
            })?;

        // Whether there is any occurrence left *after* now. Deliberately not
        // an early return: a cron whose `next_run` is already due but whose
        // expression has no further occurrence (a 7-field expression with a
        // fixed year, say) still owes exactly one final run. Returning here —
        // as this did — skipped that run entirely, persisted nothing, and left
        // the row due forever so the scheduler re-selected it every minute.
        let upcoming = schedule.upcoming(Utc).next();

        let Some(due_at) = cron.next_run else {
            // Never scheduled before (a new cron, or a row predating
            // `next_run` being maintained). Seed the due time so it stops
            // being re-selected on every sweep, and let it fire at its next
            // real occurrence rather than immediately — an unseeded cron
            // must not be treated as overdue.
            match upcoming {
                Some(upcoming) => self.persist_next_run(cron.id, upcoming).await?,
                // Never scheduled and nothing left to schedule: there is no
                // final run to owe, so retire it straight away.
                None => self.retire_cron(cron).await?,
            }
            return Ok(());
        };

        let should_run = due_at <= now;

        if should_run {
            // Advance the due time *before* executing. A failed execution
            // returns early below, and leaving `next_run` in the past would
            // then replay the same minute on every sweep.
            match upcoming {
                Some(upcoming) => self.persist_next_run(cron.id, upcoming).await?,
                // This is the final run. Park the row on the terminal
                // sentinel *before* executing, for the same reason: a failure
                // below must not replay it every minute forever.
                None => self.retire_cron(cron).await?,
            }

            // A `job` cron runs on the workload plane; everything below is
            // the `http` path, unchanged.
            if cron.mode == crons::MODE_JOB {
                return self.dispatch_job(cron, due_at, now).await;
            }

            // Execute the cron
            let start_time = std::time::Instant::now();
            let result = self.execute_cron(cron).await;
            let execution_time = start_time.elapsed().as_millis() as i32;

            // Get the deployment URL for recording the execution
            let url = format!("{}{}", self.get_deployment_url(cron).await?, cron.path);

            // Record the execution with appropriate status and error message
            let (status_code, error_message, headers) = match &result {
                Ok(response) => {
                    let header_map: std::collections::HashMap<String, String> = response
                        .headers()
                        .iter()
                        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or_default().to_string()))
                        .collect();
                    (
                        response.status().as_u16() as i32,
                        None,
                        serde_json::to_string(&header_map).unwrap_or_default(),
                    )
                }
                Err(e) => (
                    Self::recorded_status_code(e),
                    Some(e.to_string()),
                    serde_json::to_string(&std::collections::HashMap::<String, String>::new())
                        .unwrap_or_default(),
                ),
            };

            let new_execution = cron_executions::ActiveModel {
                cron_id: Set(cron.id),
                executed_at: Set(now),
                url: Set(url.clone()),
                status_code: Set(status_code),
                headers: Set(headers),
                response_time_ms: Set(execution_time),
                error_message: Set(error_message.clone()),
                ..Default::default()
            };

            new_execution.insert(self.db.as_ref()).await?;

            // A failed invocation raises an alarm, which delivers through the
            // configured notification routes, and is also published on the
            // job queue for other subscribers.
            if let Err(e) = result {
                let last_successful_run = match self.last_successful_run(cron.id).await {
                    Ok(last) => last,
                    Err(lookup_err) => {
                        warn!(
                            "Failed to look up the last successful run of cron {}: {}",
                            cron.id, lookup_err
                        );
                        None
                    }
                };

                if let Some(alarm_service) = self.alarm_service.get() {
                    let alarm_service = Arc::clone(alarm_service);
                    let request = Self::failure_alarm(cron, &e, &url, last_successful_run);
                    let (cron_id, project_id) = (cron.id, cron.project_id);
                    // Delivery waits on notification providers; the scheduler
                    // must not, or one stalled provider would hold up every
                    // later cron.
                    tokio::spawn(async move {
                        match time::timeout(
                            ALARM_DELIVERY_TIMEOUT,
                            alarm_service.fire_alarm(request),
                        )
                        .await
                        {
                            Ok(Ok(_)) => {}
                            Ok(Err(alarm_err)) => error!(
                                "Failed to raise alarm for failed cron {} in project {}: {}",
                                cron_id, project_id, alarm_err
                            ),
                            Err(_) => error!(
                                "Raising the alarm for failed cron {} in project {} did not \
                                 finish within {}s; notification delivery may be stalled",
                                cron_id,
                                project_id,
                                ALARM_DELIVERY_TIMEOUT.as_secs()
                            ),
                        }
                    });
                } else {
                    warn!(
                        "Cron {} in project {} failed but no alarm service is wired; \
                         the failure is only in the execution history",
                        cron.id, cron.project_id
                    );
                }

                let error_data = temps_core::CronInvocationErrorData {
                    project_id: cron.project_id,
                    environment_id: cron.environment_id,
                    cron_job_id: cron.id,
                    cron_job_name: cron.path.clone(),
                    error_message: e.to_string(),
                    schedule: cron.schedule.clone(),
                    timestamp: Utc::now(),
                    last_successful_run,
                };

                if let Err(queue_err) = self
                    .queue
                    .send(temps_core::Job::CronInvocationError(error_data))
                    .await
                {
                    warn!(
                        "Failed to send cron error notification for cron {}: {}",
                        cron.id, queue_err
                    );
                }
                return Err(e);
            }
        }

        Ok(())
    }

    /// Dispatch the run of `job` cron `cron` due at `due_at` to the
    /// workload plane, with Kubernetes CronJob semantics (ADR-055 §14).
    ///
    /// Every outcome is in `cron_executions`: a started run as `running`
    /// (its finalizer completes the row), a skipped one as `skipped`, and
    /// one that could not start as `failed` with why. Only a failure to
    /// reach the plane's own tables is returned as an error.
    async fn dispatch_job(
        &self,
        cron: &crons::Model,
        due_at: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Result<(), CronServiceError> {
        let Some(plane) = self.plane.get() else {
            warn!(
                cron_id = cron.id,
                project_id = cron.project_id,
                "job cron is due but no workload plane is running on this server"
            );
            return self
                .record_job_outcome(
                    cron,
                    now,
                    cron_executions::STATE_FAILED,
                    "PlaneUnavailable: no workload plane is running on this server, so job \
                     crons cannot run (HTTP crons are unaffected); check the server log for \
                     why it did not start"
                        .to_string(),
                )
                .await;
        };
        let plane_error = |operation: &'static str| {
            move |e: temps_workload_plane::PlaneError| CronServiceError::WorkloadPlane {
                cron_id: cron.id,
                operation,
                message: e.to_string(),
            }
        };

        let owner = Owner::new(CRON_OWNER_KIND, i64::from(cron.id));
        let active: Vec<i64> = plane
            .list_active(&owner)
            .await
            .map_err(plane_error("listing active runs"))?
            .into_iter()
            .map(|run| run.id)
            .collect();
        let policy = CronConcurrencyPolicy::from_stored(&cron.concurrency_policy);
        match job_dispatch_decision(policy, cron.starting_deadline_seconds, due_at, now, &active) {
            JobDispatch::Run => {}
            JobDispatch::Skip { reason, message } => {
                info!(cron_id = cron.id, reason, "job cron run skipped: {message}");
                return self
                    .record_job_outcome(
                        cron,
                        now,
                        cron_executions::STATE_SKIPPED,
                        format!("{reason}: {message}"),
                    )
                    .await;
            }
            JobDispatch::Replace { active } => {
                for workload_id in active {
                    info!(
                        cron_id = cron.id,
                        workload_id, "replacing the active run of a job cron"
                    );
                    plane
                        .delete(
                            workload_id,
                            "Replaced",
                            &format!(
                                "concurrency_policy is replace: stopped for the run due at {}",
                                due_at.to_rfc3339()
                            ),
                        )
                        .await
                        .map_err(plane_error("replacing an active run"))?;
                }
            }
        }

        let target = match self.job_target(cron).await {
            Ok(target) => target,
            Err(e) => {
                warn!(cron_id = cron.id, "job cron run could not start: {e}");
                return self
                    .record_job_outcome(
                        cron,
                        now,
                        cron_executions::STATE_FAILED,
                        format!("NoDeployment: {e}"),
                    )
                    .await;
            }
        };
        let import =
            deployment_container_import(cron.project_id, cron.environment_id, &target.container_id);
        let spec = match job_spec(cron, &target.image, import, target.container_node_id) {
            Ok(spec) => spec,
            Err(e) => {
                warn!(cron_id = cron.id, "job cron run could not start: {e}");
                return self
                    .record_job_outcome(
                        cron,
                        now,
                        cron_executions::STATE_FAILED,
                        format!("InvalidJob: {e}"),
                    )
                    .await;
            }
        };
        let request = ApplyRequest {
            owner,
            name: run_name(cron.id, due_at),
            spec,
            project_slug: Some(target.project_slug),
            finalizers: vec![RECORD_RUN_FINALIZER.to_string()],
        };
        match plane.apply(request).await {
            Ok(workload_id) => {
                insert_running(self.db.as_ref(), cron, workload_id, now).await?;
                info!(
                    cron_id = cron.id,
                    workload_id,
                    image = %target.image,
                    "job cron run dispatched"
                );
                Ok(())
            }
            // Refused at admission (already audit-logged by the plane) or
            // not stored: either way this occurrence did not run.
            Err(e) => {
                warn!(cron_id = cron.id, "job cron run was not admitted: {e}");
                self.record_job_outcome(
                    cron,
                    now,
                    cron_executions::STATE_FAILED,
                    format!("NotAdmitted: {e}"),
                )
                .await
            }
        }
    }

    /// The image, a running container (whose variables the run gets) and
    /// the project slug of `cron`'s environment's current deployment.
    async fn job_target(&self, cron: &crons::Model) -> Result<JobTarget, CronServiceError> {
        let no_deployment = || CronServiceError::NoActiveDeployment {
            env_id: cron.environment_id,
        };
        let environment = environments::Entity::find_by_id(cron.environment_id)
            .filter(environments::Column::ProjectId.eq(cron.project_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(no_deployment)?;
        let deployment_id = environment
            .current_deployment_id
            .ok_or_else(no_deployment)?;
        let deployment = deployments::Entity::find_by_id(deployment_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(no_deployment)?;
        let image = deployment
            .image_name
            .filter(|image| !image.trim().is_empty())
            .ok_or(CronServiceError::NoJobImage {
                cron_id: cron.id,
                deployment_id,
                env_id: cron.environment_id,
            })?;
        let container = deployment_containers::Entity::find()
            .filter(deployment_containers::Column::DeploymentId.eq(deployment_id))
            .filter(deployment_containers::Column::DeletedAt.is_null())
            .order_by_asc(deployment_containers::Column::Id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(no_deployment)?;
        let project = projects::Entity::find_by_id(cron.project_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(no_deployment)?;
        Ok(JobTarget {
            image,
            container_id: container.container_id,
            container_node_id: container.node_id,
            project_slug: project.slug,
        })
    }

    /// Record a `job` cron occurrence that did not become a run.
    async fn record_job_outcome(
        &self,
        cron: &crons::Model,
        now: DateTime<Utc>,
        state: &'static str,
        message: String,
    ) -> Result<(), CronServiceError> {
        let status_code = if state == cron_executions::STATE_SKIPPED {
            0
        } else {
            500
        };
        cron_executions::ActiveModel {
            cron_id: Set(cron.id),
            executed_at: Set(now),
            url: Set(cron.path.clone()),
            status_code: Set(status_code),
            headers: Set("{}".to_string()),
            response_time_ms: Set(0),
            error_message: Set(Some(message)),
            state: Set(Some(state.to_string())),
            finished_at: Set(Some(now)),
            ..Default::default()
        }
        .insert(self.db.as_ref())
        .await?;
        Ok(())
    }

    /// Status code stored for a failed invocation: the endpoint's own status
    /// when it answered, 500 when it could not be reached at all.
    fn recorded_status_code(error: &CronServiceError) -> i32 {
        match error {
            CronServiceError::ExecutionFailed { status, .. } => i32::from(*status),
            _ => 500,
        }
    }

    /// Most recent execution of `cron_id` that answered 2xx.
    async fn last_successful_run(
        &self,
        cron_id: i32,
    ) -> Result<Option<UtcDateTime>, sea_orm::DbErr> {
        Ok(cron_executions::Entity::find()
            .filter(cron_executions::Column::CronId.eq(cron_id))
            .filter(cron_executions::Column::StatusCode.between(200, 299))
            .order_by_desc(cron_executions::Column::ExecutedAt)
            .one(self.db.as_ref())
            .await?
            .map(|execution| execution.executed_at))
    }

    /// Alarm for one failed invocation. The alarm service debounces repeats
    /// per project and alarm type (5-minute cooldown), so a cron failing
    /// every minute does not page every minute.
    fn failure_alarm(
        cron: &crons::Model,
        error: &CronServiceError,
        url: &str,
        last_successful_run: Option<UtcDateTime>,
    ) -> FireAlarmRequest {
        let status = match error {
            CronServiceError::ExecutionFailed { status, .. } => Some(*status),
            _ => None,
        };
        let outcome = match status {
            Some(status) => format!("answered HTTP {status}"),
            None => "could not be reached".to_string(),
        };
        let last_success = match last_successful_run {
            Some(at) => format!("Last successful run: {}.", at.to_rfc3339()),
            None => "It has no successful run on record.".to_string(),
        };
        FireAlarmRequest {
            project_id: Some(cron.project_id),
            environment_id: Some(cron.environment_id),
            deployment_id: None,
            container_id: None,
            service_id: None,
            alarm_type: AlarmType::CronJobFailed,
            severity: AlarmSeverity::Warning,
            title: format!("Cron job {} failed", cron.path),
            message: format!(
                "Scheduled cron {} ({}, schedule '{}') {}: {}. {}",
                cron.id, cron.path, cron.schedule, outcome, error, last_success
            ),
            metadata: Some(serde_json::json!({
                "cron_id": cron.id,
                "path": cron.path,
                "schedule": cron.schedule,
                "url": url,
                "status_code": status,
                "error": error.to_string(),
                "last_successful_run": last_successful_run.map(|at| at.to_rfc3339()),
            })),
        }
    }

    async fn get_deployment_url(&self, cron: &crons::Model) -> Result<String, CronServiceError> {
        // Get the first deployment for this environment
        let deployment = deployments::Entity::find()
            .filter(deployments::Column::EnvironmentId.eq(cron.environment_id))
            .order_by_desc(deployments::Column::CreatedAt)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| CronServiceError::NoActiveDeployment {
                env_id: cron.environment_id,
            })?;

        // Get deployment containers for this deployment
        let containers = deployment_containers::Entity::find()
            .filter(deployment_containers::Column::DeploymentId.eq(deployment.id))
            .filter(deployment_containers::Column::DeletedAt.is_null())
            .all(self.db.as_ref())
            .await?;

        // Pick a random container if there's more than one, otherwise use the first
        // Use deployment ID as seed for consistent selection per deployment
        let container = if containers.len() > 1 {
            use rand::prelude::IndexedRandom;
            use rand::SeedableRng;
            let mut rng = rand::rngs::StdRng::seed_from_u64(deployment.id as u64);
            containers.choose(&mut rng)
        } else {
            containers.first()
        }
        .ok_or_else(|| CronServiceError::NoActiveDeployment {
            env_id: cron.environment_id,
        })?;

        // Use deployment mode to determine the correct host/port for container access
        let container_port =
            container
                .container_port
                .ok_or(CronServiceError::NoActiveDeployment {
                    env_id: cron.environment_id,
                })?;
        let host_port = container.host_port.unwrap_or(container_port) as u16;
        let url = temps_core::DeploymentMode::build_container_url(
            &container.container_name,
            container_port as u16,
            host_port,
            None,
        );
        Ok(url)
    }

    async fn execute_cron(
        &self,
        cron: &crons::Model,
    ) -> Result<reqwest::Response, CronServiceError> {
        let url = format!("{}{}", self.get_deployment_url(cron).await?, cron.path);
        debug!("Executing cron {} at {}", cron.id, url);

        let mut request = self.http_client.get(&url).header("X-Cron-Job", "true");

        // Attach Authorization: Bearer <CRON_SECRET> so the deployed app can verify
        // the request came from the Temps cron scheduler. The secret is the same
        // deployment token injected as CRON_SECRET into the container env vars.
        match self
            .deployment_token_service
            .get_or_create_deployment_token(cron.project_id, Some(cron.environment_id), None)
            .await
        {
            Ok(token) => {
                request = request.header("Authorization", format!("Bearer {}", token));
            }
            Err(e) => {
                warn!(
                    "Failed to get deployment token for cron {} (project {}, env {}): {}. \
                     Sending request without Authorization header.",
                    cron.id, cron.project_id, cron.environment_id, e
                );
            }
        }

        let response = request
            .send()
            .await
            .map_err(|e| CronServiceError::ExecutionError {
                cron_id: cron.id,
                url: url.clone(),
                message: format!("request failed: {}", e),
            })?;

        if !response.status().is_success() {
            return Err(CronServiceError::ExecutionFailed {
                cron_id: cron.id,
                url,
                status: response.status().as_u16(),
                message: format!("Failed with status: {}", response.status()),
            });
        }

        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The scheduler must ask the database for the crons that are due, not
    /// for the whole table. A regression here is silent — crons still fire —
    /// so the predicate is asserted directly.
    #[test]
    fn due_crons_query_selects_only_live_crons_that_are_due() {
        use sea_orm::{DbBackend, QueryTrait};

        let sql = DatabaseCronConfigService::due_crons_query(Utc::now())
            .build(DbBackend::Postgres)
            .to_string();

        assert!(
            sql.contains("\"deleted_at\" IS NULL"),
            "soft-deleted crons must not even be fetched: {sql}"
        );
        assert!(
            sql.contains("\"next_run\" IS NULL"),
            "a cron that has never been scheduled must still be selected so it can be seeded: {sql}"
        );
        assert!(
            sql.contains("\"next_run\" <="),
            "due selection must happen in SQL, not in Rust: {sql}"
        );
    }

    #[test]
    fn duration_until_next_minute_is_the_remainder_of_the_minute() {
        let at_15s = Utc
            .with_ymd_and_hms(2026, 4, 2, 10, 30, 15)
            .single()
            .expect("valid timestamp");
        assert_eq!(
            DatabaseCronConfigService::duration_until_next_minute(at_15s),
            Duration::from_secs(45)
        );

        let on_the_minute = Utc
            .with_ymd_and_hms(2026, 4, 2, 10, 30, 0)
            .single()
            .expect("valid timestamp");
        assert_eq!(
            DatabaseCronConfigService::duration_until_next_minute(on_the_minute),
            Duration::from_secs(60),
            "waking exactly on the minute must wait a full minute, not fire twice"
        );
    }

    /// Leap seconds put `second()` at 60 and `nanosecond()` above 1e9. The
    /// previous implementation subtracted its way to a negative
    /// `chrono::Duration` and panicked in `to_std()`; the sleep must stay
    /// inside one minute instead.
    #[test]
    fn duration_until_next_minute_survives_a_leap_second() {
        let leap = Utc
            .with_ymd_and_hms(2026, 6, 30, 23, 59, 59)
            .single()
            .expect("valid timestamp")
            .with_nanosecond(1_500_000_000)
            .expect("leap-second nanosecond is representable");

        let wait = DatabaseCronConfigService::duration_until_next_minute(leap);
        assert!(
            wait > Duration::ZERO && wait <= Duration::from_secs(60),
            "leap-second wait must stay within one minute, got {wait:?}"
        );
    }
    use temps_database::test_utils::TestDatabase;
    use temps_entities::upstream_config::UpstreamList;

    // Mock queue for tests
    struct MockQueue;

    #[async_trait]
    impl temps_core::JobQueue for MockQueue {
        async fn send(&self, _job: temps_core::Job) -> Result<(), temps_core::QueueError> {
            Ok(())
        }

        fn subscribe(&self) -> Box<dyn temps_core::JobReceiver> {
            unimplemented!("Not needed for tests")
        }
    }

    fn create_test_deployment_token_service(
        db: Arc<DatabaseConnection>,
    ) -> Arc<DeploymentTokenService> {
        let encryption_service = Arc::new(
            temps_core::EncryptionService::new(
                "0000000000000000000000000000000000000000000000000000000000000000",
            )
            .expect("Failed to create test encryption service"),
        );
        Arc::new(DeploymentTokenService::new(db, encryption_service))
    }

    // Helper function to create test project and environment
    async fn create_test_project_and_environment(
        db: &DatabaseConnection,
    ) -> Result<
        (
            temps_entities::projects::Model,
            temps_entities::environments::Model,
        ),
        Box<dyn std::error::Error>,
    > {
        use temps_entities::{environments, projects};

        // Create project
        let project = projects::ActiveModel {
            name: Set("test-project".to_string()),
            slug: Set("test-project".to_string()),
            repo_name: Set("test-repo".to_string()),
            repo_owner: Set("test-owner".to_string()),
            directory: Set("test-directory".to_string()),
            main_branch: Set("main".to_string()),
            preset: Set(temps_entities::preset::Preset::Static),
            ..Default::default()
        };
        let project = project.insert(db).await?;

        // Create environment
        let environment = environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("test-env".to_string()),
            slug: Set("test-env".to_string()),
            subdomain: Set("test-env".to_string()),
            host: Set("test-env.local".to_string()),
            upstreams: Set(UpstreamList::default()),
            ..Default::default()
        };
        let environment = environment.insert(db).await?;

        Ok((project, environment))
    }

    /// A 7-field expression pinned to a past year parses fine but has no
    /// upcoming occurrence — the exact shape that used to wedge the scheduler.
    const EXHAUSTED_SCHEDULE: &str = "0 0 0 1 1 * 2020";

    async fn insert_cron(
        db: &DatabaseConnection,
        project_id: i32,
        environment_id: i32,
        schedule: &str,
        next_run: Option<UtcDateTime>,
    ) -> crons::Model {
        crons::ActiveModel {
            project_id: Set(project_id),
            environment_id: Set(environment_id),
            path: Set("/api/cron/finalize".to_string()),
            schedule: Set(schedule.to_string()),
            next_run: Set(next_run),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("cron insert")
    }

    fn cron_service(db: &Arc<DatabaseConnection>) -> DatabaseCronConfigService {
        DatabaseCronConfigService::new(
            db.clone(),
            Arc::new(MockQueue),
            create_test_deployment_token_service(db.clone()),
        )
    }

    /// A row that has never been scheduled must be *seeded*, not fired. It is
    /// only in the due set because `next_run IS NULL` means "unknown", and
    /// treating unknown as overdue would fire every cron once on upgrade.
    #[tokio::test]
    async fn process_cron_seeds_a_null_next_run_without_firing() {
        let Ok(test_db) = TestDatabase::with_migrations().await else {
            println!("Docker not available, skipping");
            return;
        };
        let db = test_db.connection_arc();
        let (project, environment) = create_test_project_and_environment(db.as_ref())
            .await
            .expect("fixtures");
        let cron = insert_cron(db.as_ref(), project.id, environment.id, "0 0 * * *", None).await;

        let service = cron_service(&db);
        let now = Utc::now();
        service
            .process_cron(&cron, now)
            .await
            .expect("seeding must not error");

        let reloaded = crons::Entity::find_by_id(cron.id)
            .one(db.as_ref())
            .await
            .unwrap()
            .expect("cron still exists");
        let next_run = reloaded.next_run.expect("a NULL next_run must be seeded");
        assert!(
            next_run > now,
            "an unseeded cron must be scheduled forward, not treated as overdue: {next_run}"
        );

        let executions = cron_executions::Entity::find()
            .filter(cron_executions::Column::CronId.eq(cron.id))
            .all(db.as_ref())
            .await
            .unwrap();
        assert!(
            executions.is_empty(),
            "seeding must not fire the cron: {executions:?}"
        );

        // And it is out of the due set now, instead of being re-selected every
        // minute forever.
        let due: Vec<i32> = DatabaseCronConfigService::due_crons_query(Utc::now())
            .all(db.as_ref())
            .await
            .unwrap()
            .into_iter()
            .map(|c| c.id)
            .collect();
        assert!(!due.contains(&cron.id), "a seeded cron is no longer due");
    }

    /// The regression this PR reintroduced and then fixed: a cron whose
    /// `next_run` is due but whose expression has no *further* occurrence
    /// still owes one final run. Returning early skipped that run, persisted
    /// nothing, and left the row due every minute forever.
    #[tokio::test]
    async fn process_cron_runs_the_final_occurrence_then_retires_the_row() {
        let Ok(test_db) = TestDatabase::with_migrations().await else {
            println!("Docker not available, skipping");
            return;
        };
        let db = test_db.connection_arc();
        let (project, environment) = create_test_project_and_environment(db.as_ref())
            .await
            .expect("fixtures");

        // A deployment + container so `get_deployment_url` resolves and the
        // run is genuinely *attempted* (the request then fails against a
        // hostname that does not resolve, which is recorded as a 500).
        let deployment = deployments::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            slug: Set("final-run-deployment".to_string()),
            state: Set("deployed".to_string()),
            metadata: Set(Some(deployments::DeploymentMetadata::default())),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("deployment insert");
        deployment_containers::ActiveModel {
            deployment_id: Set(deployment.id),
            container_id: Set("final-run-container-id".to_string()),
            container_name: Set("final-run-container.invalid".to_string()),
            container_port: Set(Some(3000)),
            deployed_at: Set(Utc::now()),
            created_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("container insert");

        let due_at = Utc::now() - chrono::Duration::minutes(1);
        let cron = insert_cron(
            db.as_ref(),
            project.id,
            environment.id,
            EXHAUSTED_SCHEDULE,
            Some(due_at),
        )
        .await;

        let service = cron_service(&db);
        // The request itself fails (unresolvable host), which is reported as
        // an error — the point is that it was attempted at all.
        let _ = service.process_cron(&cron, Utc::now()).await;

        let executions = cron_executions::Entity::find()
            .filter(cron_executions::Column::CronId.eq(cron.id))
            .all(db.as_ref())
            .await
            .unwrap();
        assert_eq!(
            executions.len(),
            1,
            "the final due occurrence must actually run, not be skipped because \
             there is nothing after it"
        );

        let reloaded = crons::Entity::find_by_id(cron.id)
            .one(db.as_ref())
            .await
            .unwrap()
            .expect("cron still exists");
        assert_eq!(
            reloaded.next_run,
            Some(DatabaseCronConfigService::retired_sentinel()),
            "a cron with no further occurrence must be parked on the terminal \
             sentinel, not left due"
        );

        let due: Vec<i32> = DatabaseCronConfigService::due_crons_query(Utc::now())
            .all(db.as_ref())
            .await
            .unwrap()
            .into_iter()
            .map(|c| c.id)
            .collect();
        assert!(
            !due.contains(&cron.id),
            "a retired cron must never be re-selected by the due query"
        );
    }

    /// Same terminal state, reached from the other direction: a row that was
    /// never scheduled *and* has nothing left to schedule owes no final run,
    /// so it is retired immediately rather than seeded.
    #[tokio::test]
    async fn process_cron_retires_an_unseeded_exhausted_schedule_without_running_it() {
        let Ok(test_db) = TestDatabase::with_migrations().await else {
            println!("Docker not available, skipping");
            return;
        };
        let db = test_db.connection_arc();
        let (project, environment) = create_test_project_and_environment(db.as_ref())
            .await
            .expect("fixtures");
        let cron = insert_cron(
            db.as_ref(),
            project.id,
            environment.id,
            EXHAUSTED_SCHEDULE,
            None,
        )
        .await;

        let service = cron_service(&db);
        service
            .process_cron(&cron, Utc::now())
            .await
            .expect("retiring must not error");

        let reloaded = crons::Entity::find_by_id(cron.id)
            .one(db.as_ref())
            .await
            .unwrap()
            .expect("cron still exists");
        assert_eq!(
            reloaded.next_run,
            Some(DatabaseCronConfigService::retired_sentinel())
        );

        let executions = cron_executions::Entity::find()
            .filter(cron_executions::Column::CronId.eq(cron.id))
            .all(db.as_ref())
            .await
            .unwrap();
        assert!(
            executions.is_empty(),
            "a never-scheduled cron owes no run: {executions:?}"
        );
    }

    /// A due cron with occurrences left runs and is advanced to the next one —
    /// the ordinary case, asserted so the finite-schedule handling above
    /// cannot regress it.
    #[tokio::test]
    async fn process_cron_advances_a_recurring_schedule_past_now() {
        let Ok(test_db) = TestDatabase::with_migrations().await else {
            println!("Docker not available, skipping");
            return;
        };
        let db = test_db.connection_arc();
        let (project, environment) = create_test_project_and_environment(db.as_ref())
            .await
            .expect("fixtures");
        let cron = insert_cron(
            db.as_ref(),
            project.id,
            environment.id,
            "* * * * *",
            Some(Utc::now() - chrono::Duration::minutes(1)),
        )
        .await;

        let service = cron_service(&db);
        let now = Utc::now();
        // No deployment exists, so the run itself errors — the scheduling
        // half is what this asserts.
        let _ = service.process_cron(&cron, now).await;

        let reloaded = crons::Entity::find_by_id(cron.id)
            .one(db.as_ref())
            .await
            .unwrap()
            .expect("cron still exists");
        let next_run = reloaded.next_run.expect("next_run must be advanced");
        assert!(
            next_run > now,
            "a due cron must be advanced past now even when the run fails, \
             or the same minute replays on every sweep: {next_run}"
        );
        assert_ne!(
            Some(next_run),
            Some(DatabaseCronConfigService::retired_sentinel()),
            "a live recurring schedule must never be retired"
        );
    }

    /// The scheduler sleep must never collapse to a hot spin.
    #[test]
    fn duration_until_next_minute_is_floored_at_one_second() {
        // A leap-second instant clamps to one nanosecond before the minute
        // boundary, which used to produce a 1ns sleep.
        let leap = Utc
            .with_ymd_and_hms(2026, 6, 30, 23, 59, 59)
            .single()
            .expect("valid timestamp")
            .with_nanosecond(1_999_999_999)
            .expect("leap-second nanosecond is representable");

        assert!(
            DatabaseCronConfigService::duration_until_next_minute(leap) >= Duration::from_secs(1),
            "the scheduler must never sleep for less than a second"
        );
    }

    #[test]
    fn test_normalize_schedule_5_field() {
        // 5-field standard cron should get "0 " prepended
        assert_eq!(
            DatabaseCronConfigService::normalize_schedule("0 0 * * *"),
            "0 0 0 * * *"
        );
        assert_eq!(
            DatabaseCronConfigService::normalize_schedule("*/5 * * * *"),
            "0 */5 * * * *"
        );
        assert_eq!(
            DatabaseCronConfigService::normalize_schedule("0 9 * * 1"),
            "0 0 9 * * 1"
        );
    }

    #[test]
    fn test_normalize_schedule_6_field_passthrough() {
        // 6-field expressions should pass through unchanged
        assert_eq!(
            DatabaseCronConfigService::normalize_schedule("0 */5 * * * *"),
            "0 */5 * * * *"
        );
        assert_eq!(
            DatabaseCronConfigService::normalize_schedule("0 0 0 * * *"),
            "0 0 0 * * *"
        );
    }

    #[test]
    fn test_validate_cron_schedule_valid_6_field() {
        // 6-field schedules (explicit seconds) still work
        assert!(DatabaseCronConfigService::validate_cron_schedule("0 */5 * * * *").is_ok());
        assert!(DatabaseCronConfigService::validate_cron_schedule("0 0 * * * *").is_ok());
        assert!(DatabaseCronConfigService::validate_cron_schedule("0 0 0 * * *").is_ok());
    }

    #[test]
    fn test_validate_cron_schedule_valid_5_field() {
        // 5-field standard cron expressions (the format users write in .temps.yaml)
        assert!(DatabaseCronConfigService::validate_cron_schedule("*/5 * * * *").is_ok());
        assert!(DatabaseCronConfigService::validate_cron_schedule("0 * * * *").is_ok());
        assert!(DatabaseCronConfigService::validate_cron_schedule("0 0 * * *").is_ok());
        assert!(DatabaseCronConfigService::validate_cron_schedule("0 9 * * 1").is_ok());
        assert!(DatabaseCronConfigService::validate_cron_schedule("0 0 1 * *").is_ok());
        assert!(DatabaseCronConfigService::validate_cron_schedule("*/15 9-17 * * 1-5").is_ok());
    }

    #[test]
    fn test_validate_cron_schedule_invalid() {
        // Invalid: less than 1 minute apart (6-field)
        assert!(DatabaseCronConfigService::validate_cron_schedule("* * * * * *").is_err());

        // Invalid: less than 1 minute apart (5-field with every-second after normalization
        // won't happen since 5-field doesn't have seconds, but "* * * * *" = every minute which is ok)
        // Actually "* * * * *" = every minute, which is exactly 1 minute apart, so it passes.

        // Invalid syntax
        assert!(DatabaseCronConfigService::validate_cron_schedule("invalid").is_err());
        assert!(DatabaseCronConfigService::validate_cron_schedule("").is_err());
    }

    #[test]
    fn test_calculate_next_run_5_field() {
        // 5-field schedule should work after normalization
        let next_run = DatabaseCronConfigService::calculate_next_run("0 * * * *");
        assert!(next_run.is_ok());

        let next_run_time = next_run.unwrap();
        let now = Utc::now();
        assert!(next_run_time > now);
    }

    #[test]
    fn test_calculate_next_run_6_field() {
        // 6-field schedule should still work
        let next_run = DatabaseCronConfigService::calculate_next_run("0 0 * * * *");
        assert!(next_run.is_ok());

        let next_run_time = next_run.unwrap();
        let now = Utc::now();
        assert!(next_run_time > now);
    }

    #[tokio::test]
    async fn test_configure_crons_create_new_6_field() -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();
        let queue = Arc::new(MockQueue);
        let token_service = create_test_deployment_token_service(db.clone());

        // Create test project and environment
        let (project, environment) = create_test_project_and_environment(db.as_ref()).await?;

        let service = DatabaseCronConfigService::new(db.clone(), queue, token_service);

        let configs = vec![CronConfig {
            path: "/api/cron/cleanup".to_string(),
            schedule: "0 0 * * * *".to_string(),
            job: None,
        }];

        service
            .configure_crons(project.id, environment.id, configs)
            .await?;

        // Verify cron was created
        let crons_list = crons::Entity::find()
            .filter(crons::Column::ProjectId.eq(project.id))
            .filter(crons::Column::EnvironmentId.eq(environment.id))
            .all(db.as_ref())
            .await?;

        assert_eq!(crons_list.len(), 1);
        assert_eq!(crons_list[0].path, "/api/cron/cleanup");
        assert_eq!(crons_list[0].schedule, "0 0 * * * *");
        assert!(crons_list[0].next_run.is_some());

        Ok(())
    }

    #[tokio::test]
    async fn test_configure_crons_create_new_5_field() -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();
        let queue = Arc::new(MockQueue);
        let token_service = create_test_deployment_token_service(db.clone());

        let (project, environment) = create_test_project_and_environment(db.as_ref()).await?;

        let service = DatabaseCronConfigService::new(db.clone(), queue, token_service);

        // Use standard 5-field cron (the format users actually write in .temps.yaml)
        let configs = vec![CronConfig {
            path: "/api/cron/daily".to_string(),
            schedule: "0 0 * * *".to_string(),
            job: None,
        }];

        service
            .configure_crons(project.id, environment.id, configs)
            .await?;

        let crons_list = crons::Entity::find()
            .filter(crons::Column::ProjectId.eq(project.id))
            .filter(crons::Column::EnvironmentId.eq(environment.id))
            .all(db.as_ref())
            .await?;

        assert_eq!(crons_list.len(), 1);
        assert_eq!(crons_list[0].path, "/api/cron/daily");
        // Schedule stored as-is (5-field), normalization happens at validation/runtime
        assert_eq!(crons_list[0].schedule, "0 0 * * *");
        assert!(crons_list[0].next_run.is_some());

        Ok(())
    }

    #[tokio::test]
    async fn test_configure_crons_update_existing() -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();
        let queue = Arc::new(MockQueue);
        let token_service = create_test_deployment_token_service(db.clone());

        // Create test project and environment
        let (project, environment) = create_test_project_and_environment(db.as_ref()).await?;

        let service = DatabaseCronConfigService::new(db.clone(), queue, token_service);

        // Create initial cron
        let configs = vec![CronConfig {
            path: "/api/cron/task".to_string(),
            schedule: "0 0 * * * *".to_string(),
            job: None,
        }];
        service
            .configure_crons(project.id, environment.id, configs)
            .await?;

        // Update with different schedule
        let updated_configs = vec![CronConfig {
            path: "/api/cron/task".to_string(),
            schedule: "0 */5 * * * *".to_string(),
            job: None,
        }];
        service
            .configure_crons(project.id, environment.id, updated_configs)
            .await?;

        // Verify cron was updated
        let crons_list = crons::Entity::find()
            .filter(crons::Column::ProjectId.eq(project.id))
            .filter(crons::Column::EnvironmentId.eq(environment.id))
            .filter(crons::Column::DeletedAt.is_null())
            .all(db.as_ref())
            .await?;

        assert_eq!(crons_list.len(), 1);
        assert_eq!(crons_list[0].schedule, "0 */5 * * * *");

        Ok(())
    }

    #[tokio::test]
    async fn test_configure_crons_delete_removed() -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();
        let queue = Arc::new(MockQueue);
        let token_service = create_test_deployment_token_service(db.clone());

        // Create test project and environment
        let (project, environment) = create_test_project_and_environment(db.as_ref()).await?;

        let service = DatabaseCronConfigService::new(db.clone(), queue, token_service);

        // Create two crons
        let configs = vec![
            CronConfig {
                path: "/api/cron/task1".to_string(),
                schedule: "0 0 * * * *".to_string(),
                job: None,
            },
            CronConfig {
                path: "/api/cron/task2".to_string(),
                schedule: "0 0 * * * *".to_string(),
                job: None,
            },
        ];
        service
            .configure_crons(project.id, environment.id, configs)
            .await?;

        // Update with only one cron (task2 removed)
        let updated_configs = vec![CronConfig {
            path: "/api/cron/task1".to_string(),
            schedule: "0 0 * * * *".to_string(),
            job: None,
        }];
        service
            .configure_crons(project.id, environment.id, updated_configs)
            .await?;

        // Verify only one active cron remains
        let active_crons = crons::Entity::find()
            .filter(crons::Column::ProjectId.eq(project.id))
            .filter(crons::Column::EnvironmentId.eq(environment.id))
            .filter(crons::Column::DeletedAt.is_null())
            .all(db.as_ref())
            .await?;

        assert_eq!(active_crons.len(), 1);
        assert_eq!(active_crons[0].path, "/api/cron/task1");

        // Verify task2 is marked as deleted
        let all_crons = crons::Entity::find()
            .filter(crons::Column::ProjectId.eq(project.id))
            .filter(crons::Column::EnvironmentId.eq(environment.id))
            .all(db.as_ref())
            .await?;

        assert_eq!(all_crons.len(), 2);
        let deleted_cron = all_crons
            .iter()
            .find(|c| c.path == "/api/cron/task2")
            .unwrap();
        assert!(deleted_cron.deleted_at.is_some());

        Ok(())
    }

    #[tokio::test]
    async fn test_cron_secret_token_retrieval() -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();
        let token_service = create_test_deployment_token_service(db.clone());

        // Create test project and environment
        let (project, environment) = create_test_project_and_environment(db.as_ref()).await?;

        // First call creates a new token
        let token = token_service
            .get_or_create_deployment_token(project.id, Some(environment.id), None)
            .await
            .expect("Should create deployment token");

        assert!(!token.is_empty(), "Token should not be empty");

        // Second call should return the same token (stable across calls)
        let token_again = token_service
            .get_or_create_deployment_token(project.id, Some(environment.id), None)
            .await
            .expect("Should retrieve existing deployment token");

        assert_eq!(
            token, token_again,
            "CRON_SECRET should be stable: same token returned on subsequent calls"
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_cron_secret_differs_per_environment() -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();
        let token_service = create_test_deployment_token_service(db.clone());

        // Create test project
        let (project, env1) = create_test_project_and_environment(db.as_ref()).await?;

        // Create a second environment
        let env2 = temps_entities::environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("staging".to_string()),
            slug: Set("staging".to_string()),
            subdomain: Set("staging".to_string()),
            host: Set("staging.local".to_string()),
            upstreams: Set(UpstreamList::default()),
            ..Default::default()
        };
        let env2 = env2.insert(db.as_ref()).await?;

        let token_env1 = token_service
            .get_or_create_deployment_token(project.id, Some(env1.id), None)
            .await
            .expect("Should create token for env1");

        let token_env2 = token_service
            .get_or_create_deployment_token(project.id, Some(env2.id), None)
            .await
            .expect("Should create token for env2");

        assert_ne!(
            token_env1, token_env2,
            "Each environment should get a unique CRON_SECRET"
        );

        Ok(())
    }

    fn sample_cron() -> crons::Model {
        crons::Model {
            id: 41,
            project_id: 5,
            environment_id: 6,
            path: "/api/cron/report".to_string(),
            schedule: "*/5 * * * *".to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            next_run: None,
            deleted_at: None,
            mode: crons::MODE_HTTP.to_string(),
            command: None,
            concurrency_policy: "forbid".to_string(),
            starting_deadline_seconds: None,
            retry_limit: 0,
            timeout_seconds: None,
            ttl_seconds_after_finished: None,
        }
    }

    #[test]
    fn recorded_status_code_keeps_the_endpoint_status() {
        let failed = CronServiceError::ExecutionFailed {
            cron_id: 41,
            url: "http://app:3000/api/cron/report".to_string(),
            status: 404,
            message: "Failed with status: 404 Not Found".to_string(),
        };
        assert_eq!(
            DatabaseCronConfigService::recorded_status_code(&failed),
            404
        );

        let unreachable = CronServiceError::ExecutionError {
            cron_id: 41,
            url: "http://app:3000/api/cron/report".to_string(),
            message: "request failed: connection refused".to_string(),
        };
        assert_eq!(
            DatabaseCronConfigService::recorded_status_code(&unreachable),
            500
        );
    }

    #[test]
    fn failure_alarm_names_the_cron_status_and_last_success() {
        let cron = sample_cron();
        let error = CronServiceError::ExecutionFailed {
            cron_id: cron.id,
            url: "http://app:3000/api/cron/report".to_string(),
            status: 503,
            message: "Failed with status: 503 Service Unavailable".to_string(),
        };
        let last = Utc.with_ymd_and_hms(2026, 10, 1, 12, 0, 0).unwrap();
        let alarm = DatabaseCronConfigService::failure_alarm(
            &cron,
            &error,
            "http://app:3000/api/cron/report",
            Some(last),
        );
        assert_eq!(alarm.alarm_type, AlarmType::CronJobFailed);
        assert_eq!(alarm.project_id, Some(5));
        assert_eq!(alarm.environment_id, Some(6));
        assert_eq!(alarm.title, "Cron job /api/cron/report failed");
        assert!(
            alarm.message.contains("answered HTTP 503"),
            "{}",
            alarm.message
        );
        assert!(
            alarm.message.contains("2026-10-01T12:00:00"),
            "{}",
            alarm.message
        );
        let metadata = alarm.metadata.expect("metadata");
        assert_eq!(metadata["cron_id"], 41);
        assert_eq!(metadata["status_code"], 503);

        let unreachable = CronServiceError::ExecutionError {
            cron_id: cron.id,
            url: "http://app:3000/api/cron/report".to_string(),
            message: "request failed: connection refused".to_string(),
        };
        let alarm = DatabaseCronConfigService::failure_alarm(&cron, &unreachable, "u", None);
        assert!(
            alarm.message.contains("could not be reached"),
            "{}",
            alarm.message
        );
        assert!(
            alarm.message.contains("no successful run on record"),
            "{}",
            alarm.message
        );
        assert!(alarm.metadata.expect("metadata")["status_code"].is_null());
    }

    struct RecordingQueue(std::sync::Mutex<Vec<temps_core::Job>>);

    #[async_trait]
    impl temps_core::JobQueue for RecordingQueue {
        async fn send(&self, job: temps_core::Job) -> Result<(), temps_core::QueueError> {
            if let Ok(mut jobs) = self.0.lock() {
                jobs.push(job);
            }
            Ok(())
        }

        fn subscribe(&self) -> Box<dyn temps_core::JobReceiver> {
            unimplemented!("Not needed for tests")
        }
    }

    struct NoopNotificationService;

    #[async_trait]
    impl temps_core::notifications::NotificationService for NoopNotificationService {
        async fn send_notification(
            &self,
            _notification: temps_core::notifications::NotificationData,
        ) -> Result<(), temps_core::notifications::NotificationError> {
            Ok(())
        }
        async fn send_email(
            &self,
            _message: temps_core::notifications::EmailMessage,
        ) -> Result<(), temps_core::notifications::NotificationError> {
            Ok(())
        }
        async fn is_configured(
            &self,
        ) -> Result<bool, temps_core::notifications::NotificationError> {
            Ok(false)
        }
    }

    /// A notification provider that accepts the request and never answers.
    struct StalledNotificationService;

    #[async_trait]
    impl temps_core::notifications::NotificationService for StalledNotificationService {
        async fn send_notification(
            &self,
            _notification: temps_core::notifications::NotificationData,
        ) -> Result<(), temps_core::notifications::NotificationError> {
            std::future::pending().await
        }
        async fn send_email(
            &self,
            _message: temps_core::notifications::EmailMessage,
        ) -> Result<(), temps_core::notifications::NotificationError> {
            std::future::pending().await
        }
        async fn is_configured(
            &self,
        ) -> Result<bool, temps_core::notifications::NotificationError> {
            Ok(true)
        }
    }

    /// Wait (bounded) for the detached alarm task to record its alarm rows.
    async fn wait_for_cron_alarms(
        db: &sea_orm::DatabaseConnection,
        project_id: i32,
    ) -> Vec<temps_entities::alarms::Model> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            let alarms = temps_entities::alarms::Entity::find()
                .filter(temps_entities::alarms::Column::ProjectId.eq(project_id))
                .filter(temps_entities::alarms::Column::AlarmType.eq("cron_job_failed"))
                .all(db)
                .await
                .expect("alarm query");
            if !alarms.is_empty() || tokio::time::Instant::now() >= deadline {
                return alarms;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Serve `HTTP 503` to every connection, as a broken cron endpoint would.
    async fn failing_endpoint() -> u16 {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test endpoint");
        let port = listener.local_addr().expect("local addr").port();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let mut buffer = [0u8; 1024];
                let _ = socket.read(&mut buffer).await;
                let _ = socket
                    .write_all(
                        b"HTTP/1.1 503 Service Unavailable\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                    )
                    .await;
            }
        });
        port
    }

    /// A project, environment and cron whose deployment resolves to an
    /// endpoint that always answers `503`.
    async fn failing_cron_fixture(
        db: &Arc<DatabaseConnection>,
    ) -> (
        temps_entities::projects::Model,
        temps_entities::environments::Model,
        crons::Model,
    ) {
        let (project, environment) = create_test_project_and_environment(db.as_ref())
            .await
            .expect("fixtures");
        let port = failing_endpoint().await;

        let deployment = deployments::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            slug: Set("cron-alarm-deployment".to_string()),
            state: Set("deployed".to_string()),
            metadata: Set(Some(deployments::DeploymentMetadata::default())),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("deployment insert");
        // Both deployment modes resolve this container to 127.0.0.1:<port>
        // (Docker mode via the name, baremetal via localhost + host port).
        deployment_containers::ActiveModel {
            deployment_id: Set(deployment.id),
            container_id: Set("cron-alarm-container-id".to_string()),
            container_name: Set("127.0.0.1".to_string()),
            container_port: Set(Some(i32::from(port))),
            host_port: Set(Some(i32::from(port))),
            deployed_at: Set(Utc::now()),
            created_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("container insert");

        let cron = insert_cron(
            db.as_ref(),
            project.id,
            environment.id,
            "*/5 * * * *",
            Some(Utc::now() - chrono::Duration::minutes(1)),
        )
        .await;
        (project, environment, cron)
    }

    /// #1289: a failed invocation records the endpoint's real status, raises
    /// a cron alarm, and publishes the queue event with the last success.
    #[tokio::test]
    async fn failed_invocation_records_status_and_raises_an_alarm() {
        let Ok(test_db) = TestDatabase::with_migrations().await else {
            println!("Docker not available, skipping");
            return;
        };
        let db = test_db.connection_arc();
        let (project, environment, cron) = failing_cron_fixture(&db).await;
        let last_success = Utc::now() - chrono::Duration::hours(2);
        cron_executions::ActiveModel {
            cron_id: Set(cron.id),
            executed_at: Set(last_success),
            url: Set("http://127.0.0.1/api/cron/finalize".to_string()),
            status_code: Set(200),
            headers: Set("{}".to_string()),
            response_time_ms: Set(5),
            error_message: Set(None),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("previous execution insert");

        let queue = Arc::new(RecordingQueue(std::sync::Mutex::new(Vec::new())));
        let service = DatabaseCronConfigService::new(
            db.clone(),
            queue.clone(),
            create_test_deployment_token_service(db.clone()),
        );
        service.set_alarm_service(Arc::new(AlarmService::new(
            db.clone(),
            Arc::new(NoopNotificationService),
            queue.clone(),
        )));

        let result = service.process_cron(&cron, Utc::now()).await;
        assert!(
            matches!(
                result,
                Err(CronServiceError::ExecutionFailed { status: 503, .. })
            ),
            "{result:?}"
        );

        let latest = cron_executions::Entity::find()
            .filter(cron_executions::Column::CronId.eq(cron.id))
            .order_by_desc(cron_executions::Column::ExecutedAt)
            .one(db.as_ref())
            .await
            .unwrap()
            .expect("execution recorded");
        assert_eq!(latest.status_code, 503, "the real status, not a fixed 500");

        let alarms = wait_for_cron_alarms(db.as_ref(), project.id).await;
        assert_eq!(alarms.len(), 1, "one alarm for the failed invocation");
        assert_eq!(alarms[0].environment_id, Some(environment.id));

        let jobs = queue.0.lock().expect("queue lock");
        let event = jobs
            .iter()
            .find_map(|job| match job {
                temps_core::Job::CronInvocationError(data) => Some(data.clone()),
                _ => None,
            })
            .expect("cron failure event published");
        let recorded = event.last_successful_run.expect("last success is reported");
        assert!(
            (recorded - last_success).num_milliseconds().abs() < 1000,
            "{recorded} vs {last_success}"
        );
    }

    /// A notification provider that never answers must not hold up the
    /// scheduler: the alarm is raised off the scheduler's awaited work.
    #[tokio::test]
    async fn stalled_alarm_delivery_does_not_block_the_scheduler() {
        let Ok(test_db) = TestDatabase::with_migrations().await else {
            println!("Docker not available, skipping");
            return;
        };
        let db = test_db.connection_arc();
        let (project, _environment, cron) = failing_cron_fixture(&db).await;
        let queue = Arc::new(RecordingQueue(std::sync::Mutex::new(Vec::new())));
        let service = DatabaseCronConfigService::new(
            db.clone(),
            queue.clone(),
            create_test_deployment_token_service(db.clone()),
        );
        service.set_alarm_service(Arc::new(AlarmService::new(
            db.clone(),
            Arc::new(StalledNotificationService),
            queue.clone(),
        )));

        let result = tokio::time::timeout(
            Duration::from_secs(10),
            service.process_cron(&cron, Utc::now()),
        )
        .await
        .expect("process_cron must not wait for notification delivery");
        assert!(
            matches!(
                result,
                Err(CronServiceError::ExecutionFailed { status: 503, .. })
            ),
            "{result:?}"
        );
        // The alarm row is still recorded even though delivery never finishes.
        assert_eq!(wait_for_cron_alarms(db.as_ref(), project.id).await.len(), 1);
    }

    // ---- job crons (ADR-055 §14) ----

    fn job_config(retry_limit: u32, policy: CronConcurrencyPolicy) -> CronConfig {
        CronConfig {
            path: "job:nightly-report".to_string(),
            schedule: "0 3 * * *".to_string(),
            job: Some(crate::jobs::configure_crons::JobCronConfig {
                command: vec!["bin/report".to_string(), "--yesterday".to_string()],
                concurrency_policy: policy,
                starting_deadline_seconds: Some(300),
                retry_limit,
                timeout_seconds: Some(1800),
                ttl_seconds_after_finished: None,
            }),
        }
    }

    #[tokio::test]
    async fn configure_crons_stores_job_settings_and_updates_them_in_place() {
        let Ok(test_db) = TestDatabase::with_migrations().await else {
            println!("Docker not available, skipping");
            return;
        };
        let db = test_db.connection_arc();
        let (project, environment) = create_test_project_and_environment(db.as_ref())
            .await
            .expect("fixtures");
        let service = cron_service(&db);
        let http = CronConfig {
            path: "/api/cron/cleanup".to_string(),
            schedule: "0 0 * * *".to_string(),
            job: None,
        };

        service
            .configure_crons(
                project.id,
                environment.id,
                vec![http.clone(), job_config(1, CronConcurrencyPolicy::Forbid)],
            )
            .await
            .expect("configured");
        let rows = service
            .get_environment_crons(project.id, environment.id)
            .await
            .expect("listed");
        let http_row = rows.iter().find(|c| c.path == "/api/cron/cleanup").unwrap();
        assert_eq!(http_row.mode, crons::MODE_HTTP);
        assert_eq!(http_row.command, None);
        let job_row = rows
            .iter()
            .find(|c| c.path == "job:nightly-report")
            .unwrap();
        assert_eq!(job_row.mode, crons::MODE_JOB);
        assert_eq!(
            job_row.command,
            Some(serde_json::json!(["bin/report", "--yesterday"]))
        );
        assert_eq!(job_row.concurrency_policy, "forbid");
        assert_eq!(job_row.retry_limit, 1);
        assert_eq!(job_row.starting_deadline_seconds, Some(300));
        assert_eq!(job_row.timeout_seconds, Some(1800));

        // Same schedule, new settings: updated without moving the next run,
        // and the HTTP cron is left alone.
        service
            .configure_crons(
                project.id,
                environment.id,
                vec![http, job_config(3, CronConcurrencyPolicy::Replace)],
            )
            .await
            .expect("reconfigured");
        let rows = service
            .get_environment_crons(project.id, environment.id)
            .await
            .expect("listed");
        let updated = rows
            .iter()
            .find(|c| c.path == "job:nightly-report")
            .unwrap();
        assert_eq!(updated.id, job_row.id);
        assert_eq!(updated.retry_limit, 3);
        assert_eq!(updated.concurrency_policy, "replace");
        assert_eq!(updated.next_run, job_row.next_run);
        let http_again = rows.iter().find(|c| c.path == "/api/cron/cleanup").unwrap();
        assert_eq!(http_again.updated_at, http_row.updated_at);
    }

    async fn job_cron_fixture(
        db: &Arc<DatabaseConnection>,
        policy: CronConcurrencyPolicy,
    ) -> crons::Model {
        let (project, environment) = create_test_project_and_environment(db.as_ref())
            .await
            .expect("fixtures");
        let deployment = deployments::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            slug: Set("job-cron-deployment".to_string()),
            state: Set("deployed".to_string()),
            image_name: Set(Some("registry.example.test/shop:41".to_string())),
            metadata: Set(Some(deployments::DeploymentMetadata::default())),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("deployment insert");
        deployment_containers::ActiveModel {
            deployment_id: Set(deployment.id),
            container_id: Set("job-cron-container-id".to_string()),
            container_name: Set("job-cron-container".to_string()),
            container_port: Set(Some(3000)),
            deployed_at: Set(Utc::now()),
            created_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("container insert");
        environments::Entity::update_many()
            .col_expr(
                environments::Column::CurrentDeploymentId,
                Expr::value(deployment.id),
            )
            .filter(environments::Column::Id.eq(environment.id))
            .exec(db.as_ref())
            .await
            .expect("current deployment set");

        let service = cron_service(db);
        service
            .configure_crons(project.id, environment.id, vec![job_config(0, policy)])
            .await
            .expect("configured");
        service
            .get_environment_crons(project.id, environment.id)
            .await
            .expect("listed")
            .into_iter()
            .next()
            .expect("the job cron")
    }

    struct AcceptingAudit;

    #[async_trait]
    impl temps_core::AuditLogger for AcceptingAudit {
        async fn create_audit_log(
            &self,
            _operation: &dyn temps_core::AuditOperation,
        ) -> temps_core::anyhow::Result<()> {
            Ok(())
        }
    }

    /// A plane with no node: runs are stored and admitted, never started,
    /// which is all the dispatcher's decisions need.
    fn nodeless_plane(db: &Arc<DatabaseConnection>) -> Arc<WorkloadPlane> {
        Arc::new(WorkloadPlane::new(
            db.clone(),
            temps_workload_plane::NodeDriver(None),
            Arc::new(temps_workload_plane::NoEnvSource),
            Arc::new(AcceptingAudit),
            temps_workload_plane::PlaneConfig::new("adr055-p2-cron-test"),
        ))
    }

    async fn executions_of(db: &DatabaseConnection, cron_id: i32) -> Vec<cron_executions::Model> {
        cron_executions::Entity::find()
            .filter(cron_executions::Column::CronId.eq(cron_id))
            .order_by_asc(cron_executions::Column::Id)
            .all(db)
            .await
            .expect("executions")
    }

    #[tokio::test]
    async fn a_due_job_cron_with_no_plane_is_recorded_failed_with_the_reason() {
        let Ok(test_db) = TestDatabase::with_migrations().await else {
            println!("Docker not available, skipping");
            return;
        };
        let db = test_db.connection_arc();
        let cron = job_cron_fixture(&db, CronConcurrencyPolicy::Forbid).await;
        let service = cron_service(&db);
        let due_at = Utc::now() - chrono::Duration::seconds(5);
        service
            .dispatch_job(&cron, due_at, Utc::now())
            .await
            .expect("recorded");
        let executions = executions_of(db.as_ref(), cron.id).await;
        assert_eq!(executions.len(), 1);
        assert_eq!(
            executions[0].state.as_deref(),
            Some(cron_executions::STATE_FAILED)
        );
        assert!(executions[0]
            .error_message
            .as_deref()
            .is_some_and(|m| m.starts_with("PlaneUnavailable")));
    }

    #[tokio::test]
    async fn job_cron_runs_follow_forbid_the_starting_deadline_and_the_finalizer() {
        let Ok(test_db) = TestDatabase::with_migrations().await else {
            println!("Docker not available, skipping");
            return;
        };
        let db = test_db.connection_arc();
        let cron = job_cron_fixture(&db, CronConcurrencyPolicy::Forbid).await;
        let service = cron_service(&db);
        let plane = nodeless_plane(&db);
        service.set_workload_plane(plane.clone());

        // 1. The first occurrence becomes a run of the deployment image.
        let first_due = Utc::now() - chrono::Duration::seconds(5);
        service
            .dispatch_job(&cron, first_due, Utc::now())
            .await
            .expect("dispatched");
        let owner = Owner::new(CRON_OWNER_KIND, i64::from(cron.id));
        let active = plane.list_active(&owner).await.expect("active runs");
        assert_eq!(active.len(), 1);
        let run = &active[0];
        assert_eq!(run.name, run_name(cron.id, first_due));
        assert_eq!(
            run.spec.artifact,
            temps_workload::Artifact::oci("registry.example.test/shop:41")
        );
        assert_eq!(
            run.spec.env.imports,
            vec![deployment_container_import(
                cron.project_id,
                cron.environment_id,
                "job-cron-container-id"
            )]
        );
        assert_eq!(run.finalizers, vec![RECORD_RUN_FINALIZER.to_string()]);
        assert_eq!(
            run.spec.placement.pinned_nodes(),
            Some(&[temps_workload_plane::scheduler::CONTROL_PLANE_NODE_ID][..]),
            "the source container runs on the control plane, so the run does too"
        );
        let executions = executions_of(db.as_ref(), cron.id).await;
        assert_eq!(executions.len(), 1);
        assert_eq!(executions[0].workload_id, Some(run.id));
        assert_eq!(
            executions[0].state.as_deref(),
            Some(cron_executions::STATE_RUNNING)
        );

        // 2. The next occurrence, while the first is active: forbid skips it.
        let second_due = first_due + chrono::Duration::minutes(1);
        service
            .dispatch_job(&cron, second_due, second_due)
            .await
            .expect("skipped");
        assert_eq!(plane.list_active(&owner).await.expect("active").len(), 1);
        let executions = executions_of(db.as_ref(), cron.id).await;
        assert_eq!(executions.len(), 2);
        assert_eq!(
            executions[1].state.as_deref(),
            Some(cron_executions::STATE_SKIPPED)
        );
        assert!(executions[1]
            .error_message
            .as_deref()
            .is_some_and(|m| m.starts_with("ConcurrencyForbidden")));

        // 3. Ten minutes late with a 300s starting deadline: skipped.
        let third_due = second_due + chrono::Duration::minutes(1);
        service
            .dispatch_job(&cron, third_due, third_due + chrono::Duration::minutes(10))
            .await
            .expect("skipped");
        let executions = executions_of(db.as_ref(), cron.id).await;
        assert!(executions[2]
            .error_message
            .as_deref()
            .is_some_and(|m| m.starts_with("MissedStartingDeadline")));

        // 4. The run finishes: its finalizer completes the same row.
        let mut finished = plane.get(run.id).await.expect("the run");
        finished.status.phase = temps_workload::WorkloadPhase::Succeeded;
        finished.status.exit_code = Some(0);
        finished.attempt = 1;
        finished.finished_at = Some(Utc::now());
        finished.output_tail = Some("report written\n".to_string());
        temps_workload_plane::Finalizer::finalize(
            &crate::services::cron_jobs::CronRunFinalizer::new(db.clone()),
            &finished,
        )
        .await
        .expect("finalized");
        let executions = executions_of(db.as_ref(), cron.id).await;
        assert_eq!(
            executions.len(),
            3,
            "the run's row is completed, not duplicated"
        );
        assert_eq!(
            executions[0].state.as_deref(),
            Some(cron_executions::STATE_SUCCEEDED)
        );
        assert_eq!(executions[0].status_code, 200);
        assert_eq!(executions[0].exit_code, Some(0));
        assert_eq!(
            executions[0].output_tail.as_deref(),
            Some("report written\n")
        );
    }

    /// The variables of a run beside a worker's container are recomputed
    /// with the worker's `*.temps.local` rewrites, and those beside a
    /// control-plane container carry control-plane Docker hostnames: either
    /// way they are only right on the source's node, so the run is pinned
    /// there instead of being placed wherever the scheduler likes.
    #[tokio::test]
    async fn a_run_beside_a_worker_container_is_pinned_to_that_worker() {
        let Ok(test_db) = TestDatabase::with_migrations().await else {
            println!("Docker not available, skipping");
            return;
        };
        let db = test_db.connection_arc();
        let cron = job_cron_fixture(&db, CronConcurrencyPolicy::Allow).await;
        let worker = temps_entities::nodes::ActiveModel {
            name: Set("cron-source-worker".to_string()),
            token_hash: Set("unused".to_string()),
            token_encrypted: Set(None),
            address: Set("https://127.0.0.1:39998".to_string()),
            private_address: Set("127.0.0.1".to_string()),
            role: Set("worker".to_string()),
            status: Set("active".to_string()),
            labels: Set(serde_json::json!({})),
            capacity: Set(serde_json::json!({})),
            dns_resolver_consecutive_failures: Set(0),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("worker insert");
        deployment_containers::Entity::update_many()
            .col_expr(
                deployment_containers::Column::NodeId,
                Expr::value(Some(worker.id)),
            )
            .filter(deployment_containers::Column::ContainerId.eq("job-cron-container-id"))
            .exec(db.as_ref())
            .await
            .expect("source container moved to the worker");

        let service = cron_service(&db);
        let plane = nodeless_plane(&db);
        service.set_workload_plane(plane.clone());
        service
            .dispatch_job(&cron, Utc::now() - chrono::Duration::seconds(5), Utc::now())
            .await
            .expect("dispatched");
        let owner = Owner::new(CRON_OWNER_KIND, i64::from(cron.id));
        let active = plane.list_active(&owner).await.expect("active runs");
        assert_eq!(active.len(), 1);
        assert_eq!(
            active[0].spec.placement.pinned_nodes(),
            Some(&[worker.id][..]),
            "the run lands on the worker its variables are resolved for"
        );
    }

    #[tokio::test]
    async fn replace_deletes_the_active_run_and_starts_the_new_one() {
        let Ok(test_db) = TestDatabase::with_migrations().await else {
            println!("Docker not available, skipping");
            return;
        };
        let db = test_db.connection_arc();
        let cron = job_cron_fixture(&db, CronConcurrencyPolicy::Replace).await;
        let service = cron_service(&db);
        let plane = nodeless_plane(&db);
        service.set_workload_plane(plane.clone());
        let owner = Owner::new(CRON_OWNER_KIND, i64::from(cron.id));

        let first_due = Utc::now() - chrono::Duration::seconds(5);
        service
            .dispatch_job(&cron, first_due, Utc::now())
            .await
            .expect("dispatched");
        let first = plane.list_active(&owner).await.expect("active")[0].id;

        let second_due = first_due + chrono::Duration::minutes(1);
        service
            .dispatch_job(&cron, second_due, second_due)
            .await
            .expect("replaced");
        let active = plane.list_active(&owner).await.expect("active");
        assert_eq!(active.len(), 1);
        assert_ne!(active[0].id, first);
        assert_eq!(active[0].name, run_name(cron.id, second_due));
        let replaced = plane.get(first).await.expect("the replaced run");
        assert!(replaced.deleted_at.is_some());
        assert_eq!(
            replaced
                .status
                .condition(temps_workload::ConditionKind::Completed)
                .map(|c| c.reason.as_str()),
            Some("Replaced")
        );
    }
}
