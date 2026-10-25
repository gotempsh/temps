// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use chrono::Utc;
use futures::future::BoxFuture;
use sea_orm::sea_query::Expr;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, DbErr, EntityTrait, FromQueryResult,
    QueryFilter, QueryOrder, QuerySelect, Set,
};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use temps_config::ConfigService;
use temps_core::{Job, JobQueue, MonitorCreatedJob, UtcDateTime};
use temps_entities::{environments, status_checks, status_monitors};
use tokio::time::sleep;
use tracing::{debug, error, warn};

use super::types::{
    normalize_check_interval_seconds, validate_check_path, CreateMonitorRequest, MonitorResponse,
    MonitorStatus, StatusBucket, StatusCheckResponse, StatusPageError, UpdateMonitorRequest,
    UptimeDataPoint, UptimeHistoryResponse, DEFAULT_CHECK_INTERVAL_SECS,
};

const USER_CREATED_MONITOR_BOOTSTRAP_MESSAGE: &str =
    "Monitor created - awaiting first health check";

fn monitor_url(public_url: &str, monitor_type: &str, check_path: Option<&str>) -> String {
    let base = public_url.trim_end_matches('/');
    match check_path {
        Some("/") => base.to_string(),
        Some(path) if !path.is_empty() => format!("{base}{path}"),
        _ if monitor_type == "health" => format!("{base}/health"),
        _ => public_url.to_string(),
    }
}

/// Upper bound on the buckets one bucketed-status read may return. Every
/// bucket of the range is materialised (empty ones as "unknown"), so the
/// range/interval pair -- not the number of stored checks -- sets the
/// response size. 2 000 covers a week of 5-minute buckets.
const MAX_STATUS_BUCKETS: i64 = 2_000;

/// Bucket width in seconds for a bucketed-status `interval`, or `None` for an
/// unsupported one. All widths divide a day, so they align with
/// `time_bucket`'s default origin (a UTC midnight).
fn bucket_step_secs(interval: &str) -> Option<i64> {
    match interval {
        "1min" => Some(60),
        "5min" => Some(300),
        "hourly" => Some(3_600),
        "daily" => Some(86_400),
        _ => None,
    }
}

/// Overall status of one bucket. Priority: down > degraded > operational; a
/// bucket with no checks at all is "unknown", never "operational".
fn bucket_status(operational: i64, degraded: i64, down: i64) -> &'static str {
    if down > 0 {
        "down"
    } else if degraded > 0 {
        "degraded"
    } else if operational > 0 {
        "operational"
    } else {
        "unknown"
    }
}

fn unknown_bucket(bucket_start: UtcDateTime) -> StatusBucket {
    StatusBucket {
        bucket_start,
        status: "unknown".to_string(),
        total_checks: 0,
        operational_count: 0,
        degraded_count: 0,
        down_count: 0,
        uptime_percentage: None,
        avg_response_time_ms: None,
        min_response_time_ms: None,
        max_response_time_ms: None,
        p50_response_time_ms: None,
        p95_response_time_ms: None,
        p99_response_time_ms: None,
    }
}

/// Return one bucket per `step_secs` slot of `[start, end)`, taking the
/// observed bucket where one exists and an "unknown" placeholder otherwise.
///
/// Slots are aligned to the Unix epoch, which is what `time_bucket` produces
/// for these day-dividing widths. An observed bucket off that grid (it should
/// not happen) is kept rather than dropped.
fn fill_status_buckets(
    observed: Vec<StatusBucket>,
    start: UtcDateTime,
    end: UtcDateTime,
    step_secs: i64,
) -> Vec<StatusBucket> {
    let step = step_secs.max(1);
    let mut by_start: BTreeMap<i64, StatusBucket> = observed
        .into_iter()
        .map(|bucket| (bucket.bucket_start.timestamp(), bucket))
        .collect();
    let end_secs = end.timestamp();
    let mut slot = start.timestamp().div_euclid(step) * step;
    while slot < end_secs {
        if let Some(bucket_start) = chrono::DateTime::from_timestamp(slot, 0) {
            by_start
                .entry(slot)
                .or_insert_with(|| unknown_bucket(bucket_start));
        }
        slot += step;
    }
    by_start.into_values().collect()
}

/// How long one check result keeps vouching for the endpoint.
///
/// Twice the configured interval, plus a minute for the scheduler's 15 s
/// sweep and a probe's own retries (up to ~50 s against a hanging endpoint).
/// Beyond that, the absence of the next check means Temps was not checking,
/// and the time is unknown rather than "still up".
fn coverage_grace_secs(check_interval_seconds: i32) -> i64 {
    i64::from(normalize_check_interval_seconds(check_interval_seconds)) * 2 + 60
}

/// Time-weighted breakdown of one monitor's window, in seconds.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct UptimeCoverage {
    /// Length of the (clamped) window.
    window_secs: f64,
    /// Seconds covered by any check result.
    monitored_secs: f64,
    /// Seconds covered by an operational check result.
    up_secs: f64,
}

impl UptimeCoverage {
    fn new(window_secs: f64, monitored_secs: f64, up_secs: f64) -> Self {
        let window_secs = window_secs.max(0.0);
        let monitored_secs = monitored_secs.clamp(0.0, window_secs);
        Self {
            window_secs,
            monitored_secs,
            up_secs: up_secs.clamp(0.0, monitored_secs),
        }
    }

    /// Share of monitored time that was operational; `None` when nothing was
    /// monitored, so "no data" is never reported as 100% (or 0%).
    fn uptime_percentage(&self) -> Option<f64> {
        (self.monitored_secs > 0.0).then(|| self.up_secs / self.monitored_secs * 100.0)
    }

    fn downtime_secs(&self) -> f64 {
        self.monitored_secs - self.up_secs
    }

    fn unknown_secs(&self) -> f64 {
        self.window_secs - self.monitored_secs
    }
}

/// Roll a project's production monitors up into one status.
///
/// Counts come from `get_projects_monitor_health`; `operational_count` and
/// `major_outage_count` only cover monitors whose environment has a current
/// deployment, and `undeployed_count` covers the rest.
///
/// - `no_monitors`: the project has no active production monitor.
/// - `not_deployed`: every monitor is on an environment that has never been
///   (or is no longer) deployed, so there is nothing to check yet. This is a
///   neutral state, not an outage.
/// - `down` is reserved for every *checked* monitor being hard-down
///   (major_outage, or no check within a day); a monitor merely returning a
///   soft client error (degraded/partial_outage) does not paint the project
///   red -- see health_check_service.rs for the per-monitor statuses.
/// - `degraded`: anything in between.
fn project_monitor_status(
    monitor_count: i64,
    undeployed_count: i64,
    operational_count: i64,
    major_outage_count: i64,
) -> &'static str {
    let checked = monitor_count - undeployed_count;
    if monitor_count == 0 {
        "no_monitors"
    } else if checked <= 0 {
        "not_deployed"
    } else if operational_count == checked {
        "operational"
    } else if major_outage_count == checked {
        "down"
    } else {
        "degraded"
    }
}

fn is_managed_monitor_unique_violation(error: &DbErr) -> bool {
    let rendered = error.to_string();
    rendered.contains("idx_status_monitors_managed_environment")
        && (rendered.contains("23505") || rendered.contains("duplicate key"))
}

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
                        warn!(
                            "Failed to emit MonitorCreated event for monitor {}: {:?}",
                            monitor.id, e
                        );
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
                if let Ok(base_url) = self
                    .config_service
                    .get_deployment_url_by_slug(&env.subdomain)
                    .await
                {
                    // Use custom check_path if set, otherwise fall back to monitor_type logic
                    response.monitor_url = monitor_url(
                        &base_url,
                        &response.monitor_type,
                        response.check_path.as_deref(),
                    );
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
                        debug!(
                            "{} succeeded after {} attempts",
                            operation_name,
                            attempt + 1
                        );
                    }
                    return Ok(result);
                }
                Err(e) => {
                    let db_err: sea_orm::DbErr = e.into();

                    // Check if it's a transient error that we should retry
                    let should_retry = match &db_err {
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
                            db_err
                        );
                        last_error = Some(db_err);
                        continue;
                    }

                    // Non-retryable error or final attempt
                    error!(
                        "{} failed after {} attempts: {:?}",
                        operation_name,
                        attempt + 1,
                        db_err
                    );
                    return Err(StatusPageError::Database(db_err));
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

    /// Create a default monitor for an environment if it doesn't exist
    pub async fn ensure_monitor_for_environment(
        &self,
        project_id: i32,
        environment_id: i32,
        environment_name: &str,
    ) -> Result<MonitorResponse, StatusPageError> {
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

        let source_type = temps_entities::projects::Entity::find_by_id(project_id)
            .select_only()
            .column(temps_entities::projects::Column::SourceType)
            .into_tuple::<temps_entities::source_type::SourceType>()
            .one(self.db.as_ref())
            .await?;
        if source_type == Some(temps_entities::source_type::SourceType::External) {
            return Err(StatusPageError::InvalidRequest(format!(
                "Project {project_id} is for monitoring only; environment {environment_id} has no hosted endpoint to health-check"
            )));
        }

        // Prefer the canonical managed monitor when one already exists.
        let existing = status_monitors::Entity::find()
            .filter(status_monitors::Column::ProjectId.eq(project_id))
            .filter(status_monitors::Column::EnvironmentId.eq(Some(environment_id)))
            .filter(status_monitors::Column::IsManaged.eq(true))
            .one(self.db.as_ref())
            .await
            .map_err(|source| StatusPageError::ManagedMonitorReconciliation {
                operation: "find",
                environment_id,
                project_id,
                source,
            })?;

        if let Some(monitor) = existing {
            let response: MonitorResponse = monitor.into();
            return Ok(self.populate_monitor_url(response).await);
        }

        // This deterministic name is the reserved environment-monitor slot.
        // Adopt an existing row instead of creating a second monitor: its ID
        // owns the environment's existing uptime history, and successful
        // deployments (including `.temps.yaml` health paths) must continue
        // updating that same monitor.
        let legacy_name = format!("{} Monitor", environment_name);
        if let Some(legacy) = status_monitors::Entity::find()
            .filter(status_monitors::Column::ProjectId.eq(project_id))
            .filter(status_monitors::Column::EnvironmentId.eq(Some(environment_id)))
            .filter(status_monitors::Column::Name.eq(&legacy_name))
            .order_by_asc(status_monitors::Column::Id)
            .one(self.db.as_ref())
            .await
            .map_err(|source| StatusPageError::ManagedMonitorReconciliation {
                operation: "find the existing environment monitor for",
                environment_id,
                project_id,
                source,
            })?
        {
            let mut adopted: status_monitors::ActiveModel = legacy.into();
            adopted.is_managed = Set(true);

            let adopted = match adopted.update(self.db.as_ref()).await {
                Ok(monitor) => monitor,
                Err(update_error) if is_managed_monitor_unique_violation(&update_error) => {
                    // Environment-created jobs are at-least-once and may race
                    // startup reconciliation. The partial unique index decides
                    // the winner; if another worker established ownership,
                    // return that canonical monitor.
                    if let Some(existing) = status_monitors::Entity::find()
                        .filter(status_monitors::Column::ProjectId.eq(project_id))
                        .filter(status_monitors::Column::EnvironmentId.eq(Some(environment_id)))
                        .filter(status_monitors::Column::IsManaged.eq(true))
                        .one(self.db.as_ref())
                        .await
                        .map_err(|source| StatusPageError::ManagedMonitorReconciliation {
                            operation: "resolve a concurrent adoption of",
                            environment_id,
                            project_id,
                            source,
                        })?
                    {
                        existing
                    } else {
                        return Err(StatusPageError::ManagedMonitorReconciliation {
                            operation: "adopt",
                            environment_id,
                            project_id,
                            source: update_error,
                        });
                    }
                }
                Err(source) => {
                    return Err(StatusPageError::ManagedMonitorReconciliation {
                        operation: "adopt",
                        environment_id,
                        project_id,
                        source,
                    });
                }
            };

            tracing::info!(
                monitor_id = adopted.id,
                environment_id,
                project_id,
                "Adopted existing environment monitor"
            );

            let response: MonitorResponse = adopted.into();
            return Ok(self.populate_monitor_url(response).await);
        }

        // Create a new monitor for this environment
        let monitor = status_monitors::ActiveModel {
            project_id: Set(project_id),
            environment_id: Set(Some(environment_id)),
            name: Set(legacy_name),
            monitor_type: Set("web".to_string()),
            check_interval_seconds: Set(60), // Check every minute
            is_active: Set(true),
            is_managed: Set(true),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };

        let result = match monitor.insert(self.db.as_ref()).await {
            Ok(monitor) => monitor,
            Err(insert_error) if is_managed_monitor_unique_violation(&insert_error) => {
                // Environment creation events are at-least-once and can race.
                // The partial unique index is authoritative; if another worker
                // won, return that managed monitor instead of surfacing a false
                // deployment error.
                if let Some(existing) = status_monitors::Entity::find()
                    .filter(status_monitors::Column::ProjectId.eq(project_id))
                    .filter(status_monitors::Column::EnvironmentId.eq(Some(environment_id)))
                    .filter(status_monitors::Column::IsManaged.eq(true))
                    .one(self.db.as_ref())
                    .await
                    .map_err(|source| StatusPageError::ManagedMonitorReconciliation {
                        operation: "resolve a concurrent creation of",
                        environment_id,
                        project_id,
                        source,
                    })?
                {
                    existing
                } else {
                    return Err(StatusPageError::ManagedMonitorReconciliation {
                        operation: "create",
                        environment_id,
                        project_id,
                        source: insert_error,
                    });
                }
            }
            Err(source) => {
                return Err(StatusPageError::ManagedMonitorReconciliation {
                    operation: "create",
                    environment_id,
                    project_id,
                    source,
                });
            }
        };

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
        if let Some(ref path) = request.check_path {
            validate_check_path(path)?;
        }

        let environment_project_id = environments::Entity::find_by_id(request.environment_id)
            .select_only()
            .column(environments::Column::ProjectId)
            .into_tuple::<i32>()
            .one(self.db.as_ref())
            .await
            .map_err(|source| StatusPageError::EnvironmentOwnershipLookup {
                environment_id: request.environment_id,
                project_id,
                source,
            })?;
        if environment_project_id != Some(project_id) {
            return Err(StatusPageError::EnvironmentNotInProject {
                environment_id: request.environment_id,
                project_id,
            });
        }

        let monitor = status_monitors::ActiveModel {
            project_id: Set(project_id),
            environment_id: Set(Some(request.environment_id)),
            name: Set(request.name),
            monitor_type: Set(request.monitor_type),
            check_path: Set(request.check_path),
            // Normalise at the write boundary rather than storing the
            // request verbatim and letting the scheduler silently clamp it.
            // `0`, `-120` and `i32::MAX` all used to round-trip through the
            // API unchanged while the scheduler ran the monitor on a
            // completely different cadence, so `GET /monitors/{id}` reported
            // an interval that was never honoured. Clamped (not rejected) so
            // an import can never fail on an out-of-range value; the response
            // below carries the clamped value back to the caller.
            check_interval_seconds: Set(normalize_check_interval_seconds(
                request
                    .check_interval_seconds
                    .unwrap_or(DEFAULT_CHECK_INTERVAL_SECS),
            )),
            is_active: Set(true),
            is_managed: Set(false),
            ..Default::default()
        };

        let result = monitor.insert(self.db.as_ref()).await?;

        // Create an initial status check to bootstrap uptime calculations
        // This ensures that uptime queries have data to work with from the start
        let _initial_check = self
            .record_check(
                result.id,
                "unknown".to_string(),
                None,
                Some(USER_CREATED_MONITOR_BOOTSTRAP_MESSAGE.to_string()),
            )
            .await?;

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

    /// Update a monitor's probe path without replacing the monitor row.
    /// Keeping the same row preserves its managed ownership and status-check history.
    pub async fn update_monitor(
        &self,
        monitor_id: i32,
        request: UpdateMonitorRequest,
    ) -> Result<MonitorResponse, StatusPageError> {
        validate_check_path(&request.check_path)?;

        let result = status_monitors::Entity::update_many()
            .col_expr(
                status_monitors::Column::CheckPath,
                Expr::value(Some(request.check_path)),
            )
            .col_expr(
                status_monitors::Column::CheckPathRevision,
                Expr::col(status_monitors::Column::CheckPathRevision).add(1_i64),
            )
            .col_expr(status_monitors::Column::UpdatedAt, Expr::value(Utc::now()))
            .filter(status_monitors::Column::Id.eq(monitor_id))
            .exec(self.db.as_ref())
            .await?;
        if result.rows_affected == 0 {
            return Err(StatusPageError::NotFound);
        }

        self.get_monitor(monitor_id).await
    }

    /// Resolve the project a monitor belongs to, for project-access checks
    /// that must run before any other work on by-monitor-id routes (which
    /// carry no `project_id` in their path).
    pub async fn get_monitor_project_id(&self, monitor_id: i32) -> Result<i32, StatusPageError> {
        status_monitors::Entity::find_by_id(monitor_id)
            .select_only()
            .column(status_monitors::Column::ProjectId)
            .into_tuple::<i32>()
            .one(self.db.as_ref())
            .await?
            .ok_or(StatusPageError::NotFound)
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

    /// Get production health for multiple projects in one query.
    /// Checks only monitors linked to the "production" environment.
    /// Returns a simple status per project: operational, degraded, down,
    /// not_deployed, or no_monitors (see [`project_monitor_status`]).
    pub async fn get_projects_monitor_health(
        &self,
        project_ids: &[i32],
    ) -> Result<Vec<super::types::ProjectMonitorHealth>, StatusPageError> {
        use sea_orm::FromQueryResult;

        if project_ids.is_empty() {
            return Ok(vec![]);
        }

        // Telemetry-only projects are not deployment-health subjects, even if
        // callers include them or they have an explicitly configured monitor.
        let external_ids: std::collections::HashSet<i32> = temps_entities::projects::Entity::find()
            .select_only()
            .column(temps_entities::projects::Column::Id)
            .filter(temps_entities::projects::Column::Id.is_in(project_ids.to_vec()))
            .filter(
                temps_entities::projects::Column::SourceType
                    .eq(temps_entities::source_type::SourceType::External),
            )
            .into_tuple::<i32>()
            .all(self.db.as_ref())
            .await?
            .into_iter()
            .collect();
        let placeholders: Vec<String> = project_ids
            .iter()
            .enumerate()
            .map(|(i, _)| format!("${}", i + 1))
            .collect();
        let placeholders_str = placeholders.join(", ");

        #[derive(FromQueryResult)]
        struct ProjectHealth {
            project_id: i32,
            monitor_count: i64,
            undeployed_count: i64,
            operational_count: i64,
            major_outage_count: i64,
        }

        // Single query: for each project, get latest check status of production monitors.
        // Inlined subquery (no CTE) so Postgres can push predicates down.
        // LATERAL + LIMIT 1 uses idx_status_checks_monitor_time for index-only lookups.
        //
        // The `checked_at` lower bound is load-bearing: status_checks is a
        // TimescaleDB hypertable (1-day chunks, 90-day retention, compressed
        // after 30 days). Without a time predicate the planner cannot exclude
        // any chunk, so the LIMIT 1 merge touches every chunk — including
        // decompressing batches from every compressed one — per monitor. An
        // active monitor's latest check is at most one check interval old
        // (minutes), so a 1-day bound prunes to the newest chunks while a
        // monitor whose checker has been dead for over a day correctly counts
        // as not operational.
        //
        // A monitor on an environment with no current deployment is never
        // probed (the checker skips it), so its missing check is "nothing to
        // check yet", not an outage: it is counted separately and kept out of
        // the operational/outage tallies.
        let sql = format!(
            r#"
            SELECT
                sm.project_id,
                COUNT(sm.id) as monitor_count,
                COUNT(sm.id) FILTER (WHERE e.current_deployment_id IS NULL) as undeployed_count,
                COUNT(lc.status) FILTER (
                    WHERE e.current_deployment_id IS NOT NULL AND lc.status = 'operational'
                ) as operational_count,
                COUNT(*) FILTER (
                    WHERE e.current_deployment_id IS NOT NULL
                      AND (lc.status = 'major_outage' OR lc.status IS NULL)
                ) as major_outage_count
            FROM status_monitors sm
            JOIN environments e ON e.id = sm.environment_id
                AND e.name = 'production'
                AND e.deleted_at IS NULL
            LEFT JOIN LATERAL (
                SELECT sc.status
                FROM status_checks sc
                WHERE sc.monitor_id = sm.id
                  AND sc.checked_at > NOW() - INTERVAL '1 day'
                ORDER BY sc.checked_at DESC
                LIMIT 1
            ) lc ON true
            WHERE sm.project_id IN ({placeholders})
              AND sm.is_active = true
            GROUP BY sm.project_id
            "#,
            placeholders = placeholders_str
        );

        let db_backend = sea_orm::DatabaseBackend::Postgres;
        let values: Vec<sea_orm::Value> = project_ids.iter().map(|&id| id.into()).collect();
        let stmt = sea_orm::Statement::from_sql_and_values(db_backend, &sql, values);

        let results = status_monitors::Entity::find()
            .from_raw_sql(stmt)
            .into_model::<ProjectHealth>()
            .all(self.db.as_ref())
            .await?;

        let mut health_map: std::collections::HashMap<i32, super::types::ProjectMonitorHealth> =
            std::collections::HashMap::new();

        for r in results {
            let status = project_monitor_status(
                r.monitor_count,
                r.undeployed_count,
                r.operational_count,
                r.major_outage_count,
            );

            health_map.insert(
                r.project_id,
                super::types::ProjectMonitorHealth {
                    project_id: r.project_id,
                    status: status.to_string(),
                },
            );
        }

        let result: Vec<super::types::ProjectMonitorHealth> = project_ids
            .iter()
            .filter(|id| !external_ids.contains(id))
            .map(|&id| {
                health_map
                    .remove(&id)
                    .unwrap_or(super::types::ProjectMonitorHealth {
                        project_id: id,
                        status: "no_monitors".to_string(),
                    })
            })
            .collect();

        Ok(result)
    }

    /// Set or clear the deployment-discovered path on Temps' managed monitor.
    /// Custom-named monitors retain their independently configured endpoints.
    pub async fn update_managed_check_path_for_environment(
        &self,
        project_id: i32,
        environment_id: i32,
        check_path: Option<&str>,
    ) -> Result<(), StatusPageError> {
        if let Some(check_path) = check_path {
            validate_check_path(check_path)?;
        }

        status_monitors::Entity::update_many()
            .col_expr(
                status_monitors::Column::CheckPath,
                Expr::value(check_path.map(str::to_string)),
            )
            .col_expr(
                status_monitors::Column::CheckPathRevision,
                Expr::col(status_monitors::Column::CheckPathRevision).add(1_i64),
            )
            .col_expr(status_monitors::Column::UpdatedAt, Expr::value(Utc::now()))
            .filter(status_monitors::Column::ProjectId.eq(project_id))
            .filter(status_monitors::Column::EnvironmentId.eq(Some(environment_id)))
            .filter(status_monitors::Column::IsManaged.eq(true))
            .exec(self.db.as_ref())
            .await?;

        Ok(())
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
        if !is_active {
            super::health_check_service::clear_monitor_log_state(monitor_id);
        }
        let response: MonitorResponse = result.into();
        Ok(self.populate_monitor_url(response).await)
    }

    /// Delete a monitor
    pub async fn delete_monitor(&self, monitor_id: i32) -> Result<(), StatusPageError> {
        status_monitors::Entity::delete_by_id(monitor_id)
            .exec(self.db.as_ref())
            .await?;

        super::health_check_service::clear_monitor_log_state(monitor_id);
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

        let result = Self::with_retry(&format!("record_check for monitor {}", monitor_id), || {
            let check = check.clone();
            let db = db.clone();
            Box::pin(async move { check.insert(db.as_ref()).await })
        })
        .await?;

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

    /// Time-weighted uptime for `monitor` over `[start_time, end_time)`.
    ///
    /// Each check result vouches for the endpoint from the moment it was
    /// taken until the next check, but never for longer than
    /// [`coverage_grace_secs`]. Time no check vouches for -- the Temps server
    /// or its scheduler was down, the deployment was paused, the monitor did
    /// not exist yet -- is *unknown*, and is reported separately instead of
    /// being silently folded into "up". Counting check rows instead (the old
    /// `operational / total`) made a three-hour hole with no rows look like
    /// 100% uptime, because the missing checks were never in the denominator.
    ///
    /// The window is clamped to the monitor's creation time and to now, so a
    /// freshly created monitor (or a range ending in the future) does not
    /// report the time before it existed as unknown.
    ///
    /// Aggregated in SQL with one window function, so memory stays constant
    /// regardless of how many checks the window holds.
    async fn uptime_coverage(
        &self,
        monitor: &status_monitors::Model,
        start_time: UtcDateTime,
        end_time: UtcDateTime,
    ) -> Result<UptimeCoverage, StatusPageError> {
        let start = start_time.max(monitor.created_at);
        let end = end_time.min(Utc::now());
        if end <= start {
            return Ok(UptimeCoverage::default());
        }
        let window_secs = (end - start).num_milliseconds() as f64 / 1000.0;
        let grace_secs = coverage_grace_secs(monitor.check_interval_seconds) as f64;

        #[derive(FromQueryResult)]
        struct CoverageRow {
            up_secs: f64,
            monitored_secs: f64,
        }

        // `status != 'unknown'` excludes create_monitor's synthetic bootstrap
        // row (and a retired background-worker marker), which say nothing
        // about whether the endpoint answered. The lower bound reaches back one
        // grace period so the check taken just before `start` covers the start
        // of the window.
        let row = CoverageRow::find_by_statement(sea_orm::Statement::from_sql_and_values(
            sea_orm::DatabaseBackend::Postgres,
            r#"
            WITH checks AS (
                SELECT
                    status,
                    checked_at,
                    LEAD(checked_at) OVER (ORDER BY checked_at) AS next_at
                FROM status_checks
                WHERE monitor_id = $1
                  AND checked_at >= $2 - make_interval(secs => $4)
                  AND checked_at < $3
                  AND status != 'unknown'
            ),
            spans AS (
                SELECT
                    status,
                    EXTRACT(EPOCH FROM (
                        LEAST(COALESCE(next_at, $3), checked_at + make_interval(secs => $4), $3)
                        - GREATEST(checked_at, $2)
                    ))::float8 AS secs
                FROM checks
            )
            SELECT
                COALESCE(SUM(secs) FILTER (WHERE secs > 0 AND status = 'operational'), 0)::float8 AS up_secs,
                COALESCE(SUM(secs) FILTER (WHERE secs > 0), 0)::float8 AS monitored_secs
            FROM spans
            "#,
            vec![
                monitor.id.into(),
                start.into(),
                end.into(),
                grace_secs.into(),
            ],
        ))
        .one(self.db.as_ref())
        .await?;

        let (up_secs, monitored_secs) = row
            .map(|r| (r.up_secs, r.monitored_secs))
            .unwrap_or((0.0, 0.0));
        Ok(UptimeCoverage::new(window_secs, monitored_secs, up_secs))
    }

    async fn find_monitor_model(
        &self,
        monitor_id: i32,
    ) -> Result<status_monitors::Model, StatusPageError> {
        status_monitors::Entity::find_by_id(monitor_id)
            .one(self.db.as_ref())
            .await?
            .ok_or(StatusPageError::NotFound)
    }

    /// Time-weighted uptime percentage for a monitor over the last `days`
    /// (default 30). `None` when no check covered any of that time.
    pub async fn calculate_uptime(
        &self,
        monitor_id: i32,
        days: Option<i32>,
    ) -> Result<Option<f64>, StatusPageError> {
        let days = days.unwrap_or(30);
        let monitor = self.find_monitor_model(monitor_id).await?;
        let end = Utc::now();
        let start = end - chrono::Duration::days(days as i64);
        Ok(self
            .uptime_coverage(&monitor, start, end)
            .await?
            .uptime_percentage())
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

        // Postgres' AVG() over an integer column returns NUMERIC, not
        // double precision -- casting here keeps sqlx's try_get<f64> from
        // erroring on the type mismatch (this silently failed
        // get_monitor_status -> get_status_overview swallowed it into a
        // fake "unknown" status for every monitor, even fully healthy ones).
        let result = status_checks::Entity::find()
            .filter(status_checks::Column::MonitorId.eq(monitor_id))
            .filter(status_checks::Column::CheckedAt.gte(start_date))
            .from_raw_sql(sea_orm::Statement::from_sql_and_values(
                sea_orm::DatabaseBackend::Postgres,
                r#"
                SELECT AVG(response_time_ms)::double precision as avg_time
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

    /// Get bucketed status data by querying raw status_checks table with dynamic bucketing.
    ///
    /// Every bucket of `[start_time, end_time)` is returned. A bucket no check
    /// landed in comes back as `"unknown"` with zero checks -- skipping it (as
    /// a plain `GROUP BY` does) let the remaining buckets stretch across the
    /// timeline, so hours without any check rendered as solid green.
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
            min_response_time_ms: Option<i32>,     // MIN returns INT4 (same as column type)
            max_response_time_ms: Option<i32>,     // MAX returns INT4 (same as column type)
            p50_response_time_ms: Option<f64>,     // PERCENTILE_CONT returns DOUBLE PRECISION
            p95_response_time_ms: Option<f64>,     // PERCENTILE_CONT returns DOUBLE PRECISION
            p99_response_time_ms: Option<f64>,     // PERCENTILE_CONT returns DOUBLE PRECISION
        }

        let Some(step_secs) = bucket_step_secs(interval) else {
            return Err(StatusPageError::Validation(format!(
                "Invalid interval '{interval}' for monitor {monitor_id}. Must be '1min', '5min', 'hourly', or 'daily'"
            )));
        };
        if end_time <= start_time {
            return Err(StatusPageError::Validation(format!(
                "start_time ({start_time}) must be before end_time ({end_time}) for monitor {monitor_id}"
            )));
        }
        let span_secs = (end_time - start_time).num_seconds();
        let bucket_count = temps_core::time_window::bucket_count(span_secs, step_secs);
        if bucket_count > MAX_STATUS_BUCKETS {
            return Err(StatusPageError::Validation(format!(
                "A {interval} interval over {span_secs}s yields {bucket_count} buckets for monitor {monitor_id}, above the {MAX_STATUS_BUCKETS}-bucket limit. Use a coarser interval or a shorter range"
            )));
        }

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
                    time_bucket(make_interval(secs => {step_secs}), checked_at) AS bucket,
                    COUNT(*) as total_checks,
                    COUNT(*) FILTER (WHERE status = 'operational') as operational_count,
                    -- health_check_service.rs writes 'partial_outage'/'major_outage' for
                    -- the finer-grained cases, not just plain 'degraded'/'down' -- without
                    -- these, a bucket made entirely of real outage checks fell through to
                    -- "unknown" below instead of "down".
                    COUNT(*) FILTER (WHERE status IN ('degraded', 'partial_outage')) as degraded_count,
                    COUNT(*) FILTER (WHERE status IN ('down', 'major_outage')) as down_count,
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
                  -- excludes create_monitor's synthetic bootstrap row -- see
                  -- uptime_coverage's identical comment.
                  AND status != 'unknown'
                GROUP BY bucket
            ) sub
            ORDER BY bucket ASC
            "#
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

        let decimal_to_f64 = |d: Decimal| -> Option<f64> { d.to_string().parse::<f64>().ok() };
        let observed = results
            .into_iter()
            .map(|r| super::types::StatusBucket {
                bucket_start: r.bucket,
                status: bucket_status(r.operational_count, r.degraded_count, r.down_count)
                    .to_string(),
                total_checks: r.total_checks,
                operational_count: r.operational_count,
                degraded_count: r.degraded_count,
                down_count: r.down_count,
                uptime_percentage: (r.total_checks > 0)
                    .then(|| (r.operational_count as f64 / r.total_checks as f64) * 100.0),
                avg_response_time_ms: r.avg_response_time_ms.and_then(decimal_to_f64),
                min_response_time_ms: r.min_response_time_ms.map(|v| v as f64),
                max_response_time_ms: r.max_response_time_ms.map(|v| v as f64),
                p50_response_time_ms: r.p50_response_time_ms,
                p95_response_time_ms: r.p95_response_time_ms,
                p99_response_time_ms: r.p99_response_time_ms,
            })
            .collect();

        Ok(super::types::StatusBucketedResponse {
            monitor_id,
            interval: interval.to_string(),
            buckets: fill_status_buckets(observed, start_time, end_time, step_secs),
        })
    }

    /// Get current status and uptime metrics for a monitor over the last 24h.
    pub async fn get_current_status(
        &self,
        monitor_id: i32,
    ) -> Result<super::types::CurrentStatusResponse, StatusPageError> {
        let end_time = Utc::now();
        self.get_current_status_with_timeframes(
            monitor_id,
            end_time - chrono::Duration::hours(24),
            end_time,
        )
        .await
    }

    /// Get current status and uptime metrics for a custom time range.
    pub async fn get_current_status_with_timeframes(
        &self,
        monitor_id: i32,
        start_time: UtcDateTime,
        end_time: UtcDateTime,
    ) -> Result<super::types::CurrentStatusResponse, StatusPageError> {
        use sea_orm::prelude::Decimal;

        #[derive(FromQueryResult)]
        struct LatestStats {
            avg_response_time: Option<Decimal>,
            last_check_status: Option<String>,
            last_check_at: Option<UtcDateTime>,
        }

        let monitor = self.find_monitor_model(monitor_id).await?;

        // `status != 'unknown'` excludes create_monitor's synthetic bootstrap
        // row from the "latest status" pick -- see uptime_coverage's comment.
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
                    AND status != 'unknown'
            ),
            stats AS (
                SELECT AVG(response_time_ms) as avg_response_time
                FROM recent_checks
            ),
            latest AS (
                SELECT status, checked_at
                FROM recent_checks
                WHERE rn = 1
            )
            SELECT
                s.avg_response_time,
                l.status as last_check_status,
                l.checked_at as last_check_at
            FROM stats s
            CROSS JOIN latest l
        "#;

        let stats = LatestStats::find_by_statement(sea_orm::Statement::from_sql_and_values(
            sea_orm::DatabaseBackend::Postgres,
            query,
            vec![monitor_id.into(), start_time.into(), end_time.into()],
        ))
        .one(self.db.as_ref())
        .await?
        .unwrap_or(LatestStats {
            avg_response_time: None,
            last_check_status: None,
            last_check_at: None,
        });

        let coverage = self.uptime_coverage(&monitor, start_time, end_time).await?;

        let current_status = stats
            .last_check_status
            .unwrap_or_else(|| "unknown".to_string());

        let decimal_to_f64 = |d: Decimal| -> Option<f64> { d.to_string().parse::<f64>().ok() };

        Ok(super::types::CurrentStatusResponse {
            monitor_id,
            current_status,
            uptime_percentage: coverage.uptime_percentage(),
            avg_response_time_ms: stats.avg_response_time.and_then(decimal_to_f64),
            last_check_at: stats.last_check_at,
            monitored_seconds: coverage.monitored_secs.round() as i64,
            downtime_seconds: coverage.downtime_secs().round() as i64,
            unknown_seconds: coverage.unknown_secs().round() as i64,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ActiveModelTrait, PaginatorTrait, Set};
    use temps_database::test_utils::TestDatabase;
    use temps_entities::{environments, projects, upstream_config::UpstreamList};

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
        // Use nanoseconds for better uniqueness in parallel tests
        let nanos = chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0);
        let subdomain = format!("test-env-{}", nanos);
        let slug = format!("test-env-{}", nanos);
        let env = environments::ActiveModel {
            project_id: Set(project_id),
            name: Set(slug.clone()),
            slug: Set(slug),
            subdomain: Set(subdomain.clone()),
            host: Set(format!("{}.local", subdomain)),
            upstreams: Set(UpstreamList::default()),
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
        )
        .expect("Failed to create test config");
        Arc::new(ConfigService::new(Arc::new(config), db.clone()))
    }

    fn mock_monitor(id: i32, check_path: Option<&str>) -> status_monitors::Model {
        let now = Utc::now();
        status_monitors::Model {
            id,
            project_id: 7,
            environment_id: None,
            name: "Production monitor".to_string(),
            monitor_type: "health".to_string(),
            check_path: check_path.map(str::to_string),
            check_path_revision: 4,
            check_interval_seconds: 60,
            is_active: true,
            is_managed: true,
            next_check_at: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn explicit_root_path_displays_deployment_root_for_health_monitor() {
        assert_eq!(
            monitor_url("https://app.example.test/", "health", Some("/")),
            "https://app.example.test"
        );
        assert_eq!(
            monitor_url("https://app.example.test/", "health", None),
            "https://app.example.test/health"
        );
    }

    #[tokio::test]
    async fn update_monitor_changes_only_path_and_revision() {
        let mut after = mock_monitor(11, Some("/old"));
        after.check_path = Some("/ready".to_string());
        after.check_path_revision = 5;
        let db = Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
                .append_exec_results([sea_orm::MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 1,
                }])
                .append_query_results([vec![after]])
                .into_connection(),
        );
        let service = MonitorService::new(db.clone(), create_mock_config_service(&db));

        let updated = service
            .update_monitor(
                11,
                UpdateMonitorRequest {
                    check_path: "/ready".to_string(),
                },
            )
            .await
            .expect("valid monitor path should update");

        assert_eq!(updated.id, 11);
        assert_eq!(updated.check_path.as_deref(), Some("/ready"));
        drop(service);
        let db = Arc::try_unwrap(db).unwrap_or_else(|_| panic!("mock database still has owners"));
        let transaction_log = db.into_transaction_log();
        let rendered = transaction_log
            .iter()
            .flat_map(|transaction| transaction.statements())
            .map(|statement| format!("{statement:?}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("check_path"));
        assert!(rendered.contains("/ready"));
        assert!(rendered.contains("check_path_revision"));
        assert!(rendered.contains("+"));
    }

    #[tokio::test]
    async fn update_monitor_returns_not_found_for_missing_id() {
        let db = Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
                .append_exec_results([sea_orm::MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 0,
                }])
                .into_connection(),
        );
        let service = MonitorService::new(db.clone(), create_mock_config_service(&db));

        let result = service
            .update_monitor(
                404,
                UpdateMonitorRequest {
                    check_path: "/ready".to_string(),
                },
            )
            .await;

        assert!(matches!(result, Err(StatusPageError::NotFound)));
    }

    #[tokio::test]
    async fn update_monitor_validates_before_database_access() {
        let db = Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection(),
        );
        let service = MonitorService::new(db.clone(), create_mock_config_service(&db));

        let result = service
            .update_monitor(
                11,
                UpdateMonitorRequest {
                    check_path: "https://example.test/ready".to_string(),
                },
            )
            .await;

        assert!(matches!(result, Err(StatusPageError::Validation(_))));
        drop(service);
        let db = Arc::try_unwrap(db).unwrap_or_else(|_| panic!("mock database still has owners"));
        assert!(db.into_transaction_log().is_empty());
    }

    #[tokio::test]
    async fn update_monitor_propagates_database_failure() {
        let db = Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
                .append_exec_errors([DbErr::Custom("monitor update unavailable".to_string())])
                .into_connection(),
        );
        let service = MonitorService::new(db.clone(), create_mock_config_service(&db));

        let result = service
            .update_monitor(
                11,
                UpdateMonitorRequest {
                    check_path: "/ready".to_string(),
                },
            )
            .await;

        assert!(matches!(result, Err(StatusPageError::Database(_))));
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
            ..Default::default()
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

    /// The API must not accept an interval it has no intention of honouring.
    /// Before this, `0`/`-120`/`5` round-tripped verbatim while the scheduler
    /// silently ran the monitor on a different cadence, so the response
    /// advertised a check frequency that never happened.
    #[tokio::test]
    async fn create_monitor_clamps_out_of_range_intervals_and_returns_the_clamped_value() {
        let Ok(test_db) = TestDatabase::with_migrations().await else {
            println!("Docker not available, skipping");
            return;
        };
        let db = test_db.connection_arc();
        let config_service = create_mock_config_service(&db);
        let service = MonitorService::new(db.clone(), config_service);

        let project = create_test_project(&db).await;
        let environment = create_test_environment(&db, project.id).await;

        // (requested, expected stored + returned)
        let cases = [
            (Some(5), 30),    // below the floor -> floor
            (Some(29), 30),   // just below the floor -> floor
            (Some(0), 60),    // "unset" sentinel -> default
            (Some(-120), 60), // nonsense -> default
            (None, 60),       // omitted -> default
            (Some(600), 600), // in range -> untouched
            (Some(30), 30),   // exactly the floor -> untouched
        ];

        for (index, (requested, expected)) in cases.into_iter().enumerate() {
            let monitor = service
                .create_monitor(
                    project.id,
                    CreateMonitorRequest {
                        name: format!("clamp-{index}"),
                        monitor_type: "web".to_string(),
                        environment_id: environment.id,
                        check_interval_seconds: requested,
                        ..Default::default()
                    },
                )
                .await
                .expect("out-of-range intervals are clamped, never rejected");

            assert_eq!(
                monitor.check_interval_seconds, expected,
                "requested {requested:?} must be reported back as {expected}"
            );

            let stored = status_monitors::Entity::find_by_id(monitor.id)
                .one(db.as_ref())
                .await
                .unwrap()
                .expect("monitor was created");
            assert_eq!(
                stored.check_interval_seconds, expected,
                "requested {requested:?} must be *stored* as {expected}, not clamped later"
            );
        }
    }

    #[tokio::test]
    async fn deployment_health_path_only_updates_and_clears_the_managed_monitor() {
        let Ok(test_db) = TestDatabase::with_migrations().await else {
            println!("Docker not available, skipping");
            return;
        };
        let db = test_db.connection_arc();
        let service = MonitorService::new(db.clone(), create_mock_config_service(&db));
        let project = create_test_project(&db).await;
        let environment = create_test_environment(&db, project.id).await;
        let managed = service
            .ensure_monitor_for_environment(project.id, environment.id, &environment.name)
            .await
            .unwrap();
        let custom = service
            .create_monitor(
                project.id,
                CreateMonitorRequest {
                    name: "Custom readiness endpoint".to_string(),
                    monitor_type: "web".to_string(),
                    environment_id: environment.id,
                    check_interval_seconds: Some(60),
                    check_path: Some("/api/ready".to_string()),
                },
            )
            .await
            .unwrap();

        service
            .update_managed_check_path_for_environment(project.id, environment.id, Some("/docs"))
            .await
            .unwrap();
        let changed_managed = status_monitors::Entity::find_by_id(managed.id)
            .one(db.as_ref())
            .await
            .unwrap()
            .unwrap();
        let unchanged_custom = status_monitors::Entity::find_by_id(custom.id)
            .one(db.as_ref())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(changed_managed.check_path_revision, 1);
        assert_eq!(unchanged_custom.check_path_revision, 0);
        assert_eq!(
            service
                .get_monitor(managed.id)
                .await
                .unwrap()
                .check_path
                .as_deref(),
            Some("/docs")
        );
        assert_eq!(
            service
                .get_monitor(custom.id)
                .await
                .unwrap()
                .check_path
                .as_deref(),
            Some("/api/ready")
        );

        service
            .update_managed_check_path_for_environment(project.id, environment.id, Some("/docs"))
            .await
            .unwrap();
        let equal_value_write = status_monitors::Entity::find_by_id(managed.id)
            .one(db.as_ref())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(equal_value_write.check_path.as_deref(), Some("/docs"));
        assert_eq!(equal_value_write.check_path_revision, 2);

        service
            .update_managed_check_path_for_environment(project.id, environment.id, None)
            .await
            .unwrap();
        let cleared_managed = status_monitors::Entity::find_by_id(managed.id)
            .one(db.as_ref())
            .await
            .unwrap()
            .unwrap();
        let still_unchanged_custom = status_monitors::Entity::find_by_id(custom.id)
            .one(db.as_ref())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(cleared_managed.check_path, None);
        assert_eq!(cleared_managed.check_path_revision, 3);
        assert_eq!(still_unchanged_custom.check_path_revision, 0);
        assert_eq!(
            service.get_monitor(managed.id).await.unwrap().check_path,
            None
        );
        assert_eq!(
            service
                .get_monitor(custom.id)
                .await
                .unwrap()
                .check_path
                .as_deref(),
            Some("/api/ready")
        );
    }

    #[tokio::test]
    async fn managed_monitor_adopts_a_legacy_monitor_with_the_default_name() {
        let Ok(test_db) = TestDatabase::with_migrations().await else {
            println!("Docker not available, skipping");
            return;
        };
        let db = test_db.connection_arc();
        let service = MonitorService::new(db.clone(), create_mock_config_service(&db));
        let project = create_test_project(&db).await;
        let environment = create_test_environment(&db, project.id).await;
        let legacy_monitor = status_monitors::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(Some(environment.id)),
            name: Set(format!("{} Monitor", environment.name)),
            monitor_type: Set("web".to_string()),
            check_path: Set(Some("/legacy-health".to_string())),
            check_interval_seconds: Set(60),
            is_active: Set(true),
            is_managed: Set(false),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .unwrap();

        let managed_monitor = service
            .ensure_monitor_for_environment(project.id, environment.id, &environment.name)
            .await
            .unwrap();

        assert_eq!(managed_monitor.id, legacy_monitor.id);
        let restarted_service = MonitorService::new(db.clone(), create_mock_config_service(&db));
        let after_restart = restarted_service
            .ensure_monitor_for_environment(project.id, environment.id, &environment.name)
            .await
            .unwrap();
        assert_eq!(after_restart.id, legacy_monitor.id);
        service
            .update_managed_check_path_for_environment(
                project.id,
                environment.id,
                Some("/deployed-health"),
            )
            .await
            .unwrap();
        assert_eq!(
            service
                .get_monitor(legacy_monitor.id)
                .await
                .unwrap()
                .check_path
                .as_deref(),
            Some("/deployed-health")
        );

        let monitors = status_monitors::Entity::find()
            .filter(status_monitors::Column::EnvironmentId.eq(Some(environment.id)))
            .all(db.as_ref())
            .await
            .unwrap();
        assert_eq!(monitors.len(), 1);
        assert!(monitors[0].is_managed);
    }

    #[tokio::test]
    async fn ensure_monitor_for_environment_adopts_an_existing_default_name() {
        let Ok(test_db) = TestDatabase::with_migrations().await else {
            println!("Docker not available, skipping");
            return;
        };
        let db = test_db.connection_arc();
        let service = MonitorService::new(db.clone(), create_mock_config_service(&db));
        let project = create_test_project(&db).await;
        let environment = create_test_environment(&db, project.id).await;
        let user_monitor = service
            .create_monitor(
                project.id,
                CreateMonitorRequest {
                    name: format!("{} Monitor", environment.name),
                    monitor_type: "web".to_string(),
                    environment_id: environment.id,
                    check_interval_seconds: Some(90),
                    check_path: Some("/user-health".to_string()),
                },
            )
            .await
            .unwrap();

        let managed_monitor = service
            .ensure_monitor_for_environment(project.id, environment.id, &environment.name)
            .await
            .unwrap();

        assert_eq!(managed_monitor.id, user_monitor.id);
        assert_eq!(managed_monitor.check_path.as_deref(), Some("/user-health"));
        assert_eq!(managed_monitor.check_interval_seconds, 90);
        let managed_row = status_monitors::Entity::find_by_id(managed_monitor.id)
            .one(db.as_ref())
            .await
            .unwrap()
            .unwrap();
        assert!(managed_row.is_managed);
        service
            .update_managed_check_path_for_environment(
                project.id,
                environment.id,
                Some("/from-temps-yaml"),
            )
            .await
            .unwrap();
        let updated = service.get_monitor(user_monitor.id).await.unwrap();
        assert_eq!(updated.check_path.as_deref(), Some("/from-temps-yaml"));

        let monitor_count = status_monitors::Entity::find()
            .filter(status_monitors::Column::EnvironmentId.eq(Some(environment.id)))
            .count(db.as_ref())
            .await
            .unwrap();
        assert_eq!(monitor_count, 1);
    }

    #[tokio::test]
    async fn ensure_monitor_for_environment_rejects_an_environment_from_another_project() {
        let Ok(test_db) = TestDatabase::with_migrations().await else {
            println!("Docker not available, skipping");
            return;
        };
        let db = test_db.connection_arc();
        let service = MonitorService::new(db.clone(), create_mock_config_service(&db));
        let owning_project = create_test_project(&db).await;
        let other_project = create_test_project(&db).await;
        let environment = create_test_environment(&db, owning_project.id).await;

        let result = service
            .ensure_monitor_for_environment(other_project.id, environment.id, &environment.name)
            .await;

        assert!(matches!(
            result,
            Err(StatusPageError::EnvironmentNotInProject {
                environment_id,
                project_id,
            }) if environment_id == environment.id && project_id == other_project.id
        ));
        let monitor_count = status_monitors::Entity::find()
            .filter(status_monitors::Column::EnvironmentId.eq(Some(environment.id)))
            .count(db.as_ref())
            .await
            .unwrap();
        assert_eq!(monitor_count, 0);
    }

    #[test]
    fn managed_monitor_unique_violation_requires_the_expected_constraint() {
        let expected = DbErr::Custom(
            "database error 23505: duplicate key violates constraint \
             idx_status_monitors_managed_environment"
                .to_string(),
        );
        let unrelated = DbErr::Custom(
            "database error 23505: duplicate key violates constraint other_index".to_string(),
        );
        let connection = DbErr::Custom(
            "connection failed while reading idx_status_monitors_managed_environment".to_string(),
        );

        assert!(is_managed_monitor_unique_violation(&expected));
        assert!(!is_managed_monitor_unique_violation(&unrelated));
        assert!(!is_managed_monitor_unique_violation(&connection));
    }

    #[tokio::test]
    async fn concurrent_environment_events_create_one_managed_monitor() {
        let Ok(test_db) = TestDatabase::with_migrations().await else {
            println!("Docker not available, skipping");
            return;
        };
        let db = test_db.connection_arc();
        let service = MonitorService::new(db.clone(), create_mock_config_service(&db));
        let project = create_test_project(&db).await;
        let environment = create_test_environment(&db, project.id).await;

        let (first, second) = tokio::join!(
            service.ensure_monitor_for_environment(project.id, environment.id, &environment.name),
            service.ensure_monitor_for_environment(project.id, environment.id, &environment.name)
        );
        let first = first.unwrap();
        let second = second.unwrap();
        assert_eq!(first.id, second.id);
        let managed_count = status_monitors::Entity::find()
            .filter(status_monitors::Column::EnvironmentId.eq(Some(environment.id)))
            .filter(status_monitors::Column::IsManaged.eq(true))
            .count(db.as_ref())
            .await
            .unwrap();
        assert_eq!(managed_count, 1);
    }

    #[tokio::test]
    async fn create_monitor_rejects_environment_from_another_project_without_writing() {
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.connection_arc();
        let service = MonitorService::new(db.clone(), create_mock_config_service(&db));
        let target_project = create_test_project(&db).await;
        let other_project = create_test_project(&db).await;
        let foreign_environment = create_test_environment(&db, other_project.id).await;

        let result = service
            .create_monitor(
                target_project.id,
                CreateMonitorRequest {
                    name: "Cross-project monitor".to_string(),
                    monitor_type: "web".to_string(),
                    environment_id: foreign_environment.id,
                    check_interval_seconds: Some(60),
                    check_path: None,
                },
            )
            .await;

        assert!(matches!(
            result,
            Err(StatusPageError::EnvironmentNotInProject {
                environment_id,
                project_id,
            }) if environment_id == foreign_environment.id && project_id == target_project.id
        ));
        let persisted = status_monitors::Entity::find()
            .filter(status_monitors::Column::ProjectId.eq(target_project.id))
            .all(db.as_ref())
            .await
            .unwrap();
        assert!(persisted.is_empty(), "validation must happen before insert");
    }

    #[tokio::test]
    async fn create_monitor_rejects_missing_environment() {
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.connection_arc();
        let service = MonitorService::new(db.clone(), create_mock_config_service(&db));
        let project = create_test_project(&db).await;

        let result = service
            .create_monitor(
                project.id,
                CreateMonitorRequest {
                    name: "Missing environment".to_string(),
                    monitor_type: "web".to_string(),
                    environment_id: i32::MAX,
                    check_interval_seconds: Some(60),
                    check_path: None,
                },
            )
            .await;

        assert!(matches!(
            result,
            Err(StatusPageError::EnvironmentNotInProject {
                environment_id: i32::MAX,
                project_id,
            }) if project_id == project.id
        ));
    }

    #[tokio::test]
    async fn create_monitor_fails_closed_when_environment_lookup_fails() {
        let db = Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
                .append_query_errors([sea_orm::DbErr::Custom(
                    "environment lookup unavailable".to_string(),
                )])
                .into_connection(),
        );
        let service = MonitorService::new(db.clone(), create_mock_config_service(&db));

        let result = service
            .create_monitor(
                7,
                CreateMonitorRequest {
                    name: "Lookup failure".to_string(),
                    monitor_type: "web".to_string(),
                    environment_id: 11,
                    check_interval_seconds: Some(60),
                    check_path: None,
                },
            )
            .await;

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
            ..Default::default()
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
    async fn test_get_monitor_project_id() {
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
            ..Default::default()
        };

        let created = service.create_monitor(project.id, request).await.unwrap();
        let project_id = service.get_monitor_project_id(created.id).await.unwrap();

        assert_eq!(project_id, project.id);
    }

    #[tokio::test]
    async fn test_get_monitor_project_id_not_found() {
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.connection_arc();
        let config_service = create_mock_config_service(&db);
        let service = MonitorService::new(db.clone(), config_service);

        let result = service.get_monitor_project_id(99999).await;
        assert!(matches!(result, Err(StatusPageError::NotFound)));
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
            ..Default::default()
        };
        service.create_monitor(project.id, request1).await.unwrap();

        let request2 = CreateMonitorRequest {
            name: "Monitor 2".to_string(),
            monitor_type: "api".to_string(),
            environment_id: env2.id,
            check_interval_seconds: Some(120),
            ..Default::default()
        };
        service.create_monitor(project.id, request2).await.unwrap();

        // List all monitors for project
        let all_monitors = service.list_monitors(project.id, None).await.unwrap();
        assert_eq!(all_monitors.len(), 2);

        // List monitors for specific environment
        let env1_monitors = service
            .list_monitors(project.id, Some(env1.id))
            .await
            .unwrap();
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
            ..Default::default()
        };

        let monitor = service.create_monitor(project.id, request).await.unwrap();
        assert!(monitor.is_active);

        // Deactivate monitor
        let updated = service
            .update_monitor_status(monitor.id, false)
            .await
            .unwrap();
        assert!(!updated.is_active);

        // Reactivate monitor
        let reactivated = service
            .update_monitor_status(monitor.id, true)
            .await
            .unwrap();
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
            ..Default::default()
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

    // --- gaps and failed checks ------------------------------------------

    fn ts(secs: i64) -> UtcDateTime {
        chrono::DateTime::from_timestamp(secs, 0).unwrap()
    }

    fn observed_bucket(start: i64, operational: i64, down: i64) -> StatusBucket {
        StatusBucket {
            status: bucket_status(operational, 0, down).to_string(),
            total_checks: operational + down,
            operational_count: operational,
            down_count: down,
            uptime_percentage: Some(operational as f64 / (operational + down) as f64 * 100.0),
            ..unknown_bucket(ts(start))
        }
    }

    #[test]
    fn bucket_without_checks_is_unknown_not_operational() {
        assert_eq!(bucket_status(0, 0, 0), "unknown");
        assert_eq!(bucket_status(5, 0, 0), "operational");
        assert_eq!(bucket_status(5, 1, 0), "degraded");
        assert_eq!(bucket_status(5, 1, 1), "down");
    }

    #[test]
    fn fill_status_buckets_materialises_a_three_hour_gap() {
        // 24 hourly slots starting at an hour boundary; hours 10..13 have no rows.
        let start = 1_760_000_400; // divisible by 3600
        let observed: Vec<_> = (0..24)
            .filter(|h| !(10..13).contains(h))
            .map(|h| observed_bucket(start + h * 3_600, 60, 0))
            .collect();
        assert_eq!(observed.len(), 21);

        let filled = fill_status_buckets(observed, ts(start), ts(start + 24 * 3_600), 3_600);

        assert_eq!(filled.len(), 24, "every hour of the range is returned");
        for (h, bucket) in filled.iter().enumerate() {
            assert_eq!(bucket.bucket_start, ts(start + h as i64 * 3_600));
            if (10..13).contains(&h) {
                assert_eq!(bucket.status, "unknown", "hour {h}");
                assert_eq!(bucket.total_checks, 0);
                assert_eq!(bucket.uptime_percentage, None);
            } else {
                assert_eq!(bucket.status, "operational", "hour {h}");
            }
        }
    }

    #[test]
    fn fill_status_buckets_aligns_an_unaligned_range_and_handles_no_data() {
        // 10:37 -> 12:05 covers the 10:00, 11:00 and 12:00 slots.
        let hour = 1_760_000_400;
        let filled = fill_status_buckets(
            Vec::new(),
            ts(hour + 37 * 60),
            ts(hour + 2 * 3_600 + 5 * 60),
            3_600,
        );
        let starts: Vec<_> = filled.iter().map(|b| b.bucket_start).collect();
        assert_eq!(starts, vec![ts(hour), ts(hour + 3_600), ts(hour + 7_200)]);
        assert!(filled.iter().all(|b| b.status == "unknown"));
    }

    #[test]
    fn uptime_coverage_excludes_unknown_time_and_never_reports_no_data_as_up() {
        let none = UptimeCoverage::new(86_400.0, 0.0, 0.0);
        assert_eq!(none.uptime_percentage(), None);
        assert_eq!(none.unknown_secs(), 86_400.0);

        let gap = UptimeCoverage::new(86_400.0, 75_600.0, 75_600.0);
        assert_eq!(gap.uptime_percentage(), Some(100.0));
        assert_eq!(gap.unknown_secs(), 10_800.0);

        let outage = UptimeCoverage::new(86_400.0, 86_400.0, 75_600.0);
        assert_eq!(outage.uptime_percentage(), Some(87.5));
        assert_eq!(outage.downtime_secs(), 10_800.0);
    }

    #[test]
    fn coverage_grace_scales_with_the_check_interval() {
        assert_eq!(coverage_grace_secs(60), 180);
        assert_eq!(coverage_grace_secs(300), 660);
        // Garbage intervals use the scheduler's normalised value.
        assert_eq!(
            coverage_grace_secs(0),
            i64::from(DEFAULT_CHECK_INTERVAL_SECS) * 2 + 60
        );
    }

    /// A monitor whose row says it was created `created_at`, so the window
    /// under test is not clamped away. Returns the monitor id.
    async fn create_backdated_monitor(
        db: &Arc<DatabaseConnection>,
        created_at: UtcDateTime,
    ) -> i32 {
        let project = create_test_project(db).await;
        let environment = create_test_environment(db, project.id).await;
        let monitor = status_monitors::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(Some(environment.id)),
            name: Set("Gap test monitor".to_string()),
            monitor_type: Set("web".to_string()),
            check_interval_seconds: Set(60),
            is_active: Set(true),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .unwrap();
        status_monitors::Entity::update_many()
            .col_expr(status_monitors::Column::CreatedAt, Expr::value(created_at))
            .filter(status_monitors::Column::Id.eq(monitor.id))
            .exec(db.as_ref())
            .await
            .unwrap();
        monitor.id
    }

    /// One check per minute over `[start, start + 24h)`; minutes for which
    /// `status_at` returns `None` get no row at all.
    async fn insert_minutely_checks(
        db: &Arc<DatabaseConnection>,
        monitor_id: i32,
        start: UtcDateTime,
        status_at: impl Fn(i64) -> Option<&'static str>,
    ) {
        let rows: Vec<_> = (0..1_440)
            .filter_map(|minute| {
                status_at(minute).map(|status| status_checks::ActiveModel {
                    monitor_id: Set(monitor_id),
                    status: Set(status.to_string()),
                    response_time_ms: Set(Some(42)),
                    checked_at: Set(start + chrono::Duration::minutes(minute)),
                    ..Default::default()
                })
            })
            .collect();
        for chunk in rows.chunks(500) {
            status_checks::Entity::insert_many(chunk.to_vec())
                .exec(db.as_ref())
                .await
                .unwrap();
        }
    }

    /// An hour-aligned 24h window that ended before now, so nothing clamps it.
    fn past_day_window() -> (UtcDateTime, UtcDateTime) {
        let now = Utc::now().timestamp();
        let end = ts(now.div_euclid(3_600) * 3_600 - 3_600);
        (end - chrono::Duration::hours(24), end)
    }

    const GAP_MINUTES: std::ops::Range<i64> = 600..780; // hours 10, 11, 12

    #[tokio::test]
    async fn three_hours_without_checks_is_unknown_not_uptime() {
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.connection_arc();
        let service = MonitorService::new(db.clone(), create_mock_config_service(&db));
        let (start, end) = past_day_window();
        let monitor_id = create_backdated_monitor(&db, start - chrono::Duration::hours(1)).await;
        // Temps itself was down for three hours: no rows at all.
        insert_minutely_checks(&db, monitor_id, start, |m| {
            (!GAP_MINUTES.contains(&m)).then_some("operational")
        })
        .await;

        let status = service
            .get_current_status_with_timeframes(monitor_id, start, end)
            .await
            .unwrap();
        // The last check before the gap (minute 599) vouches for one grace
        // period (180s) instead of its usual 60s, so 120s of the three-hour
        // hole count as monitored and the rest is unknown.
        assert_eq!(status.unknown_seconds, 3 * 3_600 - 120);
        assert_eq!(status.monitored_seconds, 86_400 - status.unknown_seconds);
        assert_eq!(status.downtime_seconds, 0);
        assert_eq!(status.uptime_percentage, Some(100.0));

        let buckets = service
            .get_bucketed_status(monitor_id, "hourly", start, end)
            .await
            .unwrap()
            .buckets;
        assert_eq!(buckets.len(), 24, "gap hours must not be dropped");
        for (hour, bucket) in buckets.iter().enumerate() {
            let expected = if (10..13).contains(&hour) {
                "unknown"
            } else {
                "operational"
            };
            assert_eq!(bucket.status, expected, "hour {hour}");
        }
        assert_eq!(buckets[11].total_checks, 0);
    }

    #[tokio::test]
    async fn three_hours_of_failed_checks_count_as_downtime() {
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.connection_arc();
        let service = MonitorService::new(db.clone(), create_mock_config_service(&db));
        let (start, end) = past_day_window();
        let monitor_id = create_backdated_monitor(&db, start - chrono::Duration::hours(1)).await;
        // Temps was up and probing, the site was not: connection refused /
        // timeouts / 5xx are all persisted as major_outage.
        insert_minutely_checks(&db, monitor_id, start, |m| {
            Some(if GAP_MINUTES.contains(&m) {
                "major_outage"
            } else {
                "operational"
            })
        })
        .await;

        let status = service
            .get_current_status_with_timeframes(monitor_id, start, end)
            .await
            .unwrap();
        assert_eq!(status.unknown_seconds, 0);
        assert_eq!(status.downtime_seconds, 3 * 3_600);
        assert_eq!(status.uptime_percentage, Some(87.5));

        let buckets = service
            .get_bucketed_status(monitor_id, "hourly", start, end)
            .await
            .unwrap()
            .buckets;
        assert_eq!(buckets.len(), 24);
        assert_eq!(buckets[10].status, "down");
        assert_eq!(buckets[10].down_count, 60);
        assert_eq!(buckets[10].uptime_percentage, Some(0.0));
        assert_eq!(buckets[9].status, "operational");
    }

    #[tokio::test]
    async fn monitor_with_no_checks_reports_no_uptime_and_unknown_buckets() {
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.connection_arc();
        let service = MonitorService::new(db.clone(), create_mock_config_service(&db));
        let (start, end) = past_day_window();
        let monitor_id = create_backdated_monitor(&db, start).await;

        let status = service
            .get_current_status_with_timeframes(monitor_id, start, end)
            .await
            .unwrap();
        assert_eq!(status.uptime_percentage, None);
        assert_eq!(status.unknown_seconds, 86_400);
        assert_eq!(
            service
                .calculate_uptime(monitor_id, Some(30))
                .await
                .unwrap(),
            None
        );

        let buckets = service
            .get_bucketed_status(monitor_id, "hourly", start, end)
            .await
            .unwrap()
            .buckets;
        assert_eq!(buckets.len(), 24);
        assert!(buckets.iter().all(|b| b.status == "unknown"));
    }

    #[tokio::test]
    async fn bucketed_status_rejects_ranges_with_too_many_buckets() {
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.connection_arc();
        let service = MonitorService::new(db.clone(), create_mock_config_service(&db));
        let end = Utc::now();
        let err = service
            .get_bucketed_status(7, "1min", end - chrono::Duration::days(7), end)
            .await
            .unwrap_err();
        assert!(matches!(err, StatusPageError::Validation(ref m) if m.contains("monitor 7")));
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
            ..Default::default()
        };

        let monitor = service.create_monitor(project.id, request).await.unwrap();

        // Get current status (should have no checks yet)
        let status = service.get_current_status(monitor.id).await.unwrap();
        assert_eq!(status.monitor_id, monitor.id);
        assert_eq!(status.current_status, "unknown");
        assert_eq!(status.uptime_percentage, None);
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
            ..Default::default()
        };

        let monitor = service.create_monitor(project.id, request).await.unwrap();

        // Monitor URL should be populated (even if mock returns error, it should be an empty string)
        assert!(!monitor.monitor_url.is_empty() || monitor.monitor_url.is_empty());

        // For health monitors, URL should end with /health (if config service works)
        if !monitor.monitor_url.is_empty() {
            assert!(monitor.monitor_url.ends_with("/health"));
        }
    }

    /// A "production" environment (the only one project health reads),
    /// optionally with a current deployment like a project that has shipped.
    async fn create_production_environment(
        db: &Arc<DatabaseConnection>,
        project_id: i32,
        deployed: bool,
    ) -> environments::Model {
        let nanos = chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0);
        let subdomain = format!("test-prod-{}", nanos);
        let environment = environments::ActiveModel {
            project_id: Set(project_id),
            name: Set("production".to_string()),
            slug: Set(subdomain.clone()),
            subdomain: Set(subdomain.clone()),
            host: Set(format!("{}.local", subdomain)),
            upstreams: Set(UpstreamList::default()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .unwrap();
        if !deployed {
            return environment;
        }
        let deployment = temps_entities::deployments::ActiveModel {
            project_id: Set(project_id),
            environment_id: Set(environment.id),
            slug: Set(format!("{subdomain}-deployment")),
            state: Set("completed".to_string()),
            metadata: Set(Some(
                temps_entities::deployments::DeploymentMetadata::default(),
            )),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .unwrap();
        let mut active: environments::ActiveModel = environment.into();
        active.current_deployment_id = Set(Some(deployment.id));
        active.update(db.as_ref()).await.unwrap()
    }

    /// A project that has never been deployed has a managed production
    /// monitor but no check rows (the checker skips undeployed environments).
    /// That is "nothing to check yet", not an outage.
    #[tokio::test]
    async fn test_project_health_never_deployed_is_not_deployed() {
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.connection_arc();
        let config_service = create_mock_config_service(&db);
        let service = MonitorService::new(db.clone(), config_service);

        let project = create_test_project(&db).await;
        let environment = create_production_environment(&db, project.id, false).await;
        let request = CreateMonitorRequest {
            name: "Production Monitor".to_string(),
            monitor_type: "web".to_string(),
            environment_id: environment.id,
            check_interval_seconds: Some(60),
            ..Default::default()
        };
        service.create_monitor(project.id, request).await.unwrap();

        let health = service
            .get_projects_monitor_health(&[project.id])
            .await
            .unwrap();

        assert_eq!(health.len(), 1);
        assert_eq!(health[0].status, "not_deployed");
    }

    #[tokio::test]
    async fn test_project_health_degraded_monitor_is_not_reported_as_down() {
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.connection_arc();
        let config_service = create_mock_config_service(&db);
        let service = MonitorService::new(db.clone(), config_service);

        let project = create_test_project(&db).await;

        let environment = create_production_environment(&db, project.id, true).await;

        let request = CreateMonitorRequest {
            name: "Production Monitor".to_string(),
            monitor_type: "web".to_string(),
            environment_id: environment.id,
            check_interval_seconds: Some(60),
            ..Default::default()
        };
        let monitor = service.create_monitor(project.id, request).await.unwrap();

        // A monitor that's merely degraded (e.g. a soft 4xx) should not be
        // indistinguishable from a fully unreachable one at the project level.
        service
            .record_check(monitor.id, "degraded".to_string(), Some(120), None)
            .await
            .unwrap();

        let health = service
            .get_projects_monitor_health(&[project.id])
            .await
            .unwrap();

        assert_eq!(health.len(), 1);
        assert_eq!(health[0].project_id, project.id);
        assert_eq!(health[0].status, "degraded");
    }

    #[tokio::test]
    async fn test_project_health_major_outage_monitor_is_reported_as_down() {
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.connection_arc();
        let config_service = create_mock_config_service(&db);
        let service = MonitorService::new(db.clone(), config_service);

        let project = create_test_project(&db).await;

        let environment = create_production_environment(&db, project.id, true).await;

        let request = CreateMonitorRequest {
            name: "Production Monitor".to_string(),
            monitor_type: "web".to_string(),
            environment_id: environment.id,
            check_interval_seconds: Some(60),
            ..Default::default()
        };
        let monitor = service.create_monitor(project.id, request).await.unwrap();

        service
            .record_check(
                monitor.id,
                "major_outage".to_string(),
                None,
                Some("connection refused".to_string()),
            )
            .await
            .unwrap();

        let health = service
            .get_projects_monitor_health(&[project.id])
            .await
            .unwrap();

        assert_eq!(health.len(), 1);
        assert_eq!(health[0].status, "down");
    }

    // ── project_monitor_status ──────────────────────────────────────────────

    #[test]
    fn never_deployed_project_is_not_deployed_rather_than_down() {
        // One managed production monitor, environment never deployed: the
        // checker skips it, so there is no check row at all.
        assert_eq!(project_monitor_status(1, 1, 0, 0), "not_deployed");
    }

    #[test]
    fn project_without_monitors_is_no_monitors() {
        assert_eq!(project_monitor_status(0, 0, 0, 0), "no_monitors");
    }

    #[test]
    fn deployed_project_with_all_checks_passing_is_operational() {
        assert_eq!(project_monitor_status(1, 0, 1, 0), "operational");
    }

    #[test]
    fn deployed_project_with_all_checks_failing_is_down() {
        assert_eq!(project_monitor_status(2, 0, 0, 2), "down");
    }

    /// An undeployed monitor alongside a healthy deployed one must not drag
    /// the project to "degraded".
    #[test]
    fn undeployed_monitors_do_not_count_against_deployed_ones() {
        assert_eq!(project_monitor_status(2, 1, 1, 0), "operational");
        assert_eq!(project_monitor_status(2, 1, 0, 1), "down");
    }

    #[test]
    fn mixed_results_are_degraded() {
        assert_eq!(project_monitor_status(3, 0, 1, 1), "degraded");
    }
}
