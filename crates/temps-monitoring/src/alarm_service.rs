//! Alarm service for creating, resolving, and querying alarms
//!
//! Alarms are scoped to project/environment/deployment and represent
//! actionable events: container restarts, outages, high resource usage,
//! deployment failures, etc.

use chrono::{Duration, Utc};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseBackend, DatabaseConnection,
    EntityTrait, Order, PaginatorTrait, QueryFilter, QueryOrder, Set, Statement,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use temps_core::jobs::{AlarmFiredJob, AlarmResolvedJob};
use temps_core::notifications::{
    NotificationData, NotificationPriority, NotificationService, NotificationType,
};
use temps_core::{Job, JobQueue};
use temps_entities::alarms;
use thiserror::Error;
use tracing::{error, info};

/// Alarm types that the system can fire
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AlarmType {
    ContainerRestart,
    ContainerOomKilled,
    /// Container exited or died for a reason other than OOM (non-zero exit code,
    /// killed by signal, Docker reported error, etc.).
    ContainerCrash,
    HighResponseTime,
    Outage,
    HighCpu,
    HighMemory,
    DeploymentFailed,
    HealthCheckFailed,
    /// A postgres/redis/mongodb metric crossed the configured threshold.
    DatabaseMetricThreshold,
    /// A container cpu/memory metric crossed the configured threshold.
    ContainerMetricThreshold,
    /// An OTLP app metric crossed the configured threshold.
    DeploymentMetricThreshold,
    /// A node cpu/disk/memory metric crossed the configured threshold.
    NodeMetricThreshold,
}

impl AlarmType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ContainerRestart => "container_restart",
            Self::ContainerOomKilled => "container_oom_killed",
            Self::ContainerCrash => "container_crash",
            Self::HighResponseTime => "high_response_time",
            Self::Outage => "outage",
            Self::HighCpu => "high_cpu",
            Self::HighMemory => "high_memory",
            Self::DeploymentFailed => "deployment_failed",
            Self::HealthCheckFailed => "health_check_failed",
            Self::DatabaseMetricThreshold => "database_metric_threshold",
            Self::ContainerMetricThreshold => "container_metric_threshold",
            Self::DeploymentMetricThreshold => "deployment_metric_threshold",
            Self::NodeMetricThreshold => "node_metric_threshold",
        }
    }

    pub fn parse_alarm_type(s: &str) -> Option<Self> {
        match s {
            "container_restart" => Some(Self::ContainerRestart),
            "container_oom_killed" => Some(Self::ContainerOomKilled),
            "container_crash" => Some(Self::ContainerCrash),
            "high_response_time" => Some(Self::HighResponseTime),
            "outage" => Some(Self::Outage),
            "high_cpu" => Some(Self::HighCpu),
            "high_memory" => Some(Self::HighMemory),
            "deployment_failed" => Some(Self::DeploymentFailed),
            "health_check_failed" => Some(Self::HealthCheckFailed),
            "database_metric_threshold" => Some(Self::DatabaseMetricThreshold),
            "container_metric_threshold" => Some(Self::ContainerMetricThreshold),
            "deployment_metric_threshold" => Some(Self::DeploymentMetricThreshold),
            "node_metric_threshold" => Some(Self::NodeMetricThreshold),
            _ => None,
        }
    }
}

/// Alarm severity levels
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AlarmSeverity {
    Info,
    Warning,
    Critical,
}

impl AlarmSeverity {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Critical => "critical",
        }
    }

    pub fn to_notification_priority(&self) -> NotificationPriority {
        match self {
            Self::Info => NotificationPriority::Low,
            Self::Warning => NotificationPriority::High,
            Self::Critical => NotificationPriority::Critical,
        }
    }

    pub fn to_notification_type(&self) -> NotificationType {
        match self {
            Self::Info => NotificationType::Info,
            Self::Warning => NotificationType::Warning,
            Self::Critical => NotificationType::Error,
        }
    }
}

/// Alarm status
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlarmStatus {
    Firing,
    Acknowledged,
    Resolved,
}

impl AlarmStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Firing => "firing",
            Self::Acknowledged => "acknowledged",
            Self::Resolved => "resolved",
        }
    }
}

/// Request to fire a new alarm.
///
/// `environment_id` and `deployment_id` are optional because service-scoped
/// (database) alarms — fired by `AlertEvaluator` for rules with a `service_id`
/// and no `deployment_id` — have no environment or deployment to point at.
/// Container, outage, and deployment-scoped alarms always provide `Some(...)`.
#[derive(Debug, Clone)]
pub struct FireAlarmRequest {
    pub project_id: i32,
    pub environment_id: Option<i32>,
    pub deployment_id: Option<i32>,
    pub container_id: Option<i32>,
    /// External service that triggered this alarm, for service-scoped
    /// (database metric) alarms. `None` for container/outage/deployment alarms.
    pub service_id: Option<i32>,
    pub alarm_type: AlarmType,
    pub severity: AlarmSeverity,
    pub title: String,
    pub message: String,
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Error)]
pub enum AlarmError {
    #[error("Alarm {alarm_id} not found in project {project_id}")]
    NotFound { alarm_id: i32, project_id: i32 },

    #[error("Database error while {operation}: {reason}")]
    Database { operation: String, reason: String },

    #[error("Notification error for alarm {alarm_id}: {reason}")]
    Notification { alarm_id: i32, reason: String },

    #[error("Queue error for alarm {alarm_id}: {reason}")]
    Queue { alarm_id: i32, reason: String },
}

impl From<sea_orm::DbErr> for AlarmError {
    fn from(error: sea_orm::DbErr) -> Self {
        AlarmError::Database {
            operation: "alarm query".to_string(),
            reason: error.to_string(),
        }
    }
}

/// Alarm service handles creating, resolving, and querying alarms.
/// Uses the existing NotificationService for alerting and JobQueue for event propagation.
pub struct AlarmService {
    db: Arc<DatabaseConnection>,
    notification_service: Arc<dyn NotificationService>,
    job_queue: Arc<dyn JobQueue>,
    /// Minimum time between firing identical alarms (same type + deployment + container)
    cooldown: Duration,
}

impl AlarmService {
    pub fn new(
        db: Arc<DatabaseConnection>,
        notification_service: Arc<dyn NotificationService>,
        job_queue: Arc<dyn JobQueue>,
    ) -> Self {
        Self {
            db,
            notification_service,
            job_queue,
            cooldown: Duration::minutes(5),
        }
    }

    pub fn with_cooldown(mut self, cooldown: Duration) -> Self {
        self.cooldown = cooldown;
        self
    }

    /// Fire a new alarm. Checks cooldown to avoid spam.
    /// Returns the alarm ID if created, None if suppressed by cooldown.
    ///
    /// Sends the alarm's individual notification (Slack/email/webhook). To persist
    /// an alarm row + emit `Job::AlarmFired` WITHOUT the individual notification
    /// (e.g. when a caller sends one combined digest instead), use
    /// [`Self::fire_alarm_silent`].
    pub async fn fire_alarm(&self, request: FireAlarmRequest) -> Result<Option<i32>, AlarmError> {
        self.fire_alarm_internal(request, true).await
    }

    /// Fire a new alarm WITHOUT sending its individual notification, but otherwise
    /// identical to [`Self::fire_alarm`] (same cooldown check, same alarm-row
    /// insert, same `Job::AlarmFired` emission — job consumers like dashboards and
    /// webhooks still react per-alarm). Used when the caller suppresses the N
    /// per-alarm notifications in favour of a single combined digest
    /// ([`Self::send_digest_notification`]).
    pub async fn fire_alarm_silent(
        &self,
        request: FireAlarmRequest,
    ) -> Result<Option<i32>, AlarmError> {
        self.fire_alarm_internal(request, false).await
    }

    /// Shared fire path for [`Self::fire_alarm`] (`notify = true`) and
    /// [`Self::fire_alarm_silent`] (`notify = false`). `notify` gates ONLY the
    /// human-facing individual notification; the cooldown check, alarm-row insert,
    /// and `Job::AlarmFired` emission happen identically in both cases.
    async fn fire_alarm_internal(
        &self,
        request: FireAlarmRequest,
        notify: bool,
    ) -> Result<Option<i32>, AlarmError> {
        // Check cooldown: is there a recent firing alarm of the same type for the same deployment+container?
        if self.is_in_cooldown(&request).await? {
            info!(
                "Alarm suppressed by cooldown: type={}, deployment={:?}, container={:?}",
                request.alarm_type.as_str(),
                request.deployment_id,
                request.container_id
            );
            return Ok(None);
        }

        let now = Utc::now();

        let alarm = alarms::ActiveModel {
            project_id: Set(request.project_id),
            environment_id: Set(request.environment_id),
            deployment_id: Set(request.deployment_id),
            container_id: Set(request.container_id),
            service_id: Set(request.service_id),
            alarm_type: Set(request.alarm_type.as_str().to_string()),
            severity: Set(request.severity.as_str().to_string()),
            status: Set(AlarmStatus::Firing.as_str().to_string()),
            title: Set(request.title.clone()),
            message: Set(Some(request.message.clone())),
            metadata: Set(request.metadata.clone()),
            fired_at: Set(now),
            ..Default::default()
        };

        let result = alarm
            .insert(self.db.as_ref())
            .await
            .map_err(|e| AlarmError::Database {
                operation: format!(
                    "insert alarm type={} for deployment {:?}",
                    request.alarm_type.as_str(),
                    request.deployment_id
                ),
                reason: e.to_string(),
            })?;

        let alarm_id = result.id;

        info!(
            "Alarm fired: id={}, type={}, severity={}, project={}, env={:?}, deployment={:?}",
            alarm_id,
            request.alarm_type.as_str(),
            request.severity.as_str(),
            request.project_id,
            request.environment_id,
            request.deployment_id,
        );

        // Send the individual notification via the existing notification system,
        // unless the caller suppressed it (e.g. to send one combined digest).
        if notify {
            self.send_alarm_notification(&request, alarm_id).await;
        }

        // Emit job to queue so webhooks, dashboard, etc. can react
        let job = Job::AlarmFired(AlarmFiredJob {
            alarm_id,
            project_id: request.project_id,
            environment_id: request.environment_id,
            deployment_id: request.deployment_id,
            alarm_type: request.alarm_type.as_str().to_string(),
            severity: request.severity.as_str().to_string(),
            title: request.title,
        });

        if let Err(e) = self.job_queue.send(job).await {
            error!(
                "Failed to emit AlarmFired job for alarm {}: {}",
                alarm_id, e
            );
        }

        Ok(Some(alarm_id))
    }

    /// Resolve an alarm by ID
    pub async fn resolve_alarm(&self, alarm_id: i32, project_id: i32) -> Result<(), AlarmError> {
        let alarm = alarms::Entity::find_by_id(alarm_id)
            .filter(alarms::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await?
            .ok_or(AlarmError::NotFound {
                alarm_id,
                project_id,
            })?;

        if alarm.status == AlarmStatus::Resolved.as_str() {
            return Ok(());
        }

        let mut active: alarms::ActiveModel = alarm.clone().into();
        active.status = Set(AlarmStatus::Resolved.as_str().to_string());
        active.resolved_at = Set(Some(Utc::now()));
        active
            .update(self.db.as_ref())
            .await
            .map_err(|e| AlarmError::Database {
                operation: format!("resolve alarm {}", alarm_id),
                reason: e.to_string(),
            })?;

        info!("Alarm resolved: id={}, type={}", alarm_id, alarm.alarm_type);

        // Emit resolved job
        let job = Job::AlarmResolved(AlarmResolvedJob {
            alarm_id,
            project_id: alarm.project_id,
            environment_id: alarm.environment_id,
            deployment_id: alarm.deployment_id,
            alarm_type: alarm.alarm_type.clone(),
            title: alarm.title.clone(),
        });

        if let Err(e) = self.job_queue.send(job).await {
            error!(
                "Failed to emit AlarmResolved job for alarm {}: {}",
                alarm_id, e
            );
        }

        // Send recovery notification
        self.send_resolved_notification(&alarm).await;

        Ok(())
    }

    /// Resolve all firing alarms of a given type for a deployment.
    ///
    /// Uses a single batch `UPDATE … WHERE id IN (…)` inside a transaction
    /// instead of N individual `resolve_alarm` calls, which avoids both the
    /// N+1 query problem and the partial-failure risk (some resolved, some not).
    ///
    /// Post-update, a single `AlarmResolved` job and resolved notification are
    /// emitted for each alarm to maintain integration compatibility.
    pub async fn resolve_alarms_by_type(
        &self,
        project_id: i32,
        deployment_id: i32,
        alarm_type: AlarmType,
    ) -> Result<Vec<i32>, AlarmError> {
        use sea_orm::TransactionTrait;

        // Fetch the IDs and titles of all firing alarms for this type in one query.
        let firing_alarms = alarms::Entity::find()
            .filter(alarms::Column::ProjectId.eq(project_id))
            .filter(alarms::Column::DeploymentId.eq(deployment_id))
            .filter(alarms::Column::AlarmType.eq(alarm_type.as_str()))
            .filter(alarms::Column::Status.eq(AlarmStatus::Firing.as_str()))
            .all(self.db.as_ref())
            .await?;

        if firing_alarms.is_empty() {
            return Ok(Vec::new());
        }

        let ids: Vec<i32> = firing_alarms.iter().map(|a| a.id).collect();
        let now = Utc::now();

        // Batch-update all matching alarms to resolved in a single transaction.
        let txn = self.db.begin().await.map_err(|e| AlarmError::Database {
            operation: format!(
                "begin transaction for resolve_alarms_by_type type={} deployment={}",
                alarm_type.as_str(),
                deployment_id
            ),
            reason: e.to_string(),
        })?;

        // Build IN clause from the IDs we already fetched — no string injection
        // risk since all values are i32 (not user-controlled strings).
        let id_list = ids
            .iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join(", ");

        let update_sql = format!(
            "UPDATE alarms SET status = 'resolved', resolved_at = '{now}' \
             WHERE id IN ({id_list}) AND status = 'firing'",
            now = now.to_rfc3339(),
            id_list = id_list,
        );

        txn.execute(Statement::from_string(
            DatabaseBackend::Postgres,
            update_sql,
        ))
        .await
        .map_err(|e| AlarmError::Database {
            operation: format!(
                "batch resolve alarms type={} deployment={}",
                alarm_type.as_str(),
                deployment_id
            ),
            reason: e.to_string(),
        })?;

        txn.commit().await.map_err(|e| AlarmError::Database {
            operation: format!(
                "commit resolve_alarms_by_type type={} deployment={}",
                alarm_type.as_str(),
                deployment_id
            ),
            reason: e.to_string(),
        })?;

        info!(
            "Resolved {} alarm(s) of type={} for deployment={}",
            ids.len(),
            alarm_type.as_str(),
            deployment_id
        );

        // Emit resolved jobs and notifications for each alarm (non-fatal on failure).
        for alarm in &firing_alarms {
            let job = Job::AlarmResolved(AlarmResolvedJob {
                alarm_id: alarm.id,
                project_id: alarm.project_id,
                environment_id: alarm.environment_id,
                deployment_id: alarm.deployment_id,
                alarm_type: alarm.alarm_type.clone(),
                title: alarm.title.clone(),
            });

            if let Err(e) = self.job_queue.send(job).await {
                error!(
                    "Failed to emit AlarmResolved job for alarm {}: {}",
                    alarm.id, e
                );
            }

            self.send_resolved_notification(alarm).await;
        }

        Ok(ids)
    }

    /// Acknowledge an alarm (mark it as seen but not resolved)
    pub async fn acknowledge_alarm(
        &self,
        alarm_id: i32,
        project_id: i32,
        user_id: i32,
    ) -> Result<(), AlarmError> {
        let alarm = alarms::Entity::find_by_id(alarm_id)
            .filter(alarms::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await?
            .ok_or(AlarmError::NotFound {
                alarm_id,
                project_id,
            })?;

        if alarm.status != AlarmStatus::Firing.as_str() {
            return Ok(());
        }

        let mut active: alarms::ActiveModel = alarm.into();
        active.status = Set(AlarmStatus::Acknowledged.as_str().to_string());
        active.acknowledged_at = Set(Some(Utc::now()));
        active.acknowledged_by = Set(Some(user_id));
        active
            .update(self.db.as_ref())
            .await
            .map_err(|e| AlarmError::Database {
                operation: format!("acknowledge alarm {}", alarm_id),
                reason: e.to_string(),
            })?;

        info!("Alarm acknowledged: id={}, by user {}", alarm_id, user_id);

        Ok(())
    }

    /// List alarms for a project with optional filters
    pub async fn list_alarms(
        &self,
        project_id: i32,
        filters: AlarmFilters,
        page: u64,
        page_size: u64,
    ) -> Result<(Vec<alarms::Model>, u64), AlarmError> {
        let page_size = std::cmp::min(page_size, 100);

        let mut query = alarms::Entity::find().filter(alarms::Column::ProjectId.eq(project_id));

        if let Some(environment_id) = filters.environment_id {
            query = query.filter(alarms::Column::EnvironmentId.eq(environment_id));
        }

        if let Some(deployment_id) = filters.deployment_id {
            query = query.filter(alarms::Column::DeploymentId.eq(deployment_id));
        }

        if let Some(alarm_type) = &filters.alarm_type {
            query = query.filter(alarms::Column::AlarmType.eq(alarm_type.as_str()));
        }

        if let Some(status) = &filters.status {
            query = query.filter(alarms::Column::Status.eq(status.as_str()));
        }

        if let Some(severity) = &filters.severity {
            query = query.filter(alarms::Column::Severity.eq(severity.as_str()));
        }

        let paginator = query
            .order_by(alarms::Column::FiredAt, Order::Desc)
            .paginate(self.db.as_ref(), page_size);

        let total = paginator.num_items().await?;
        let items = paginator.fetch_page(page.saturating_sub(1)).await?;

        Ok((items, total))
    }

    /// Get alarm counts by status for a project (for dashboard summary widget).
    ///
    /// Uses a single aggregating SQL query (`GROUP BY status, severity`) instead
    /// of loading all non-resolved alarms into memory, which is unsafe at scale.
    pub async fn get_alarm_summary(&self, project_id: i32) -> Result<AlarmSummary, AlarmError> {
        // Aggregate by status + severity in one round-trip.
        let status_severity_sql = format!(
            "SELECT status, severity, COUNT(*)::bigint AS cnt \
             FROM alarms \
             WHERE project_id = {project_id} \
               AND status != 'resolved' \
             GROUP BY status, severity",
            project_id = project_id,
        );

        let ss_rows = self
            .db
            .query_all(Statement::from_string(
                DatabaseBackend::Postgres,
                status_severity_sql,
            ))
            .await
            .map_err(|e| AlarmError::Database {
                operation: "get_alarm_summary status/severity aggregation".to_string(),
                reason: e.to_string(),
            })?;

        // Aggregate by alarm_type in one round-trip.
        let type_sql = format!(
            "SELECT alarm_type, COUNT(*)::bigint AS cnt \
             FROM alarms \
             WHERE project_id = {project_id} \
               AND status != 'resolved' \
             GROUP BY alarm_type",
            project_id = project_id,
        );

        let type_rows = self
            .db
            .query_all(Statement::from_string(DatabaseBackend::Postgres, type_sql))
            .await
            .map_err(|e| AlarmError::Database {
                operation: "get_alarm_summary alarm_type aggregation".to_string(),
                reason: e.to_string(),
            })?;

        let mut summary = AlarmSummary::default();

        for row in ss_rows {
            let status: String = row
                .try_get("", "status")
                .map_err(|e| AlarmError::Database {
                    operation: "get_alarm_summary status read".to_string(),
                    reason: e.to_string(),
                })?;
            let severity: String =
                row.try_get("", "severity")
                    .map_err(|e| AlarmError::Database {
                        operation: "get_alarm_summary severity read".to_string(),
                        reason: e.to_string(),
                    })?;
            let cnt: i64 = row.try_get("", "cnt").map_err(|e| AlarmError::Database {
                operation: "get_alarm_summary count read".to_string(),
                reason: e.to_string(),
            })?;
            let cnt = cnt as u32;

            if status == AlarmStatus::Firing.as_str() {
                summary.firing += cnt;
            } else if status == AlarmStatus::Acknowledged.as_str() {
                summary.acknowledged += cnt;
            }

            if severity == AlarmSeverity::Critical.as_str() {
                summary.critical += cnt;
            } else if severity == AlarmSeverity::Warning.as_str() {
                summary.warning += cnt;
            }
        }

        summary.total_active = summary.firing + summary.acknowledged;

        for row in type_rows {
            let alarm_type: String =
                row.try_get("", "alarm_type")
                    .map_err(|e| AlarmError::Database {
                        operation: "get_alarm_summary alarm_type read".to_string(),
                        reason: e.to_string(),
                    })?;
            let cnt: i64 = row.try_get("", "cnt").map_err(|e| AlarmError::Database {
                operation: "get_alarm_summary type count read".to_string(),
                reason: e.to_string(),
            })?;
            summary.by_type.insert(alarm_type, cnt as u32);
        }

        Ok(summary)
    }

    /// Check if there's a recent alarm of the same type still within cooldown.
    ///
    /// When `deployment_id` or `container_id` is `None` (service-scoped alarms
    /// have no deployment; deployment-scoped alarms have no specific
    /// container), the filter explicitly requires `IS NULL` so alarms scoped
    /// at one level don't suppress alarms scoped at a different level.
    async fn is_in_cooldown(&self, request: &FireAlarmRequest) -> Result<bool, AlarmError> {
        let cutoff = Utc::now() - self.cooldown;

        let mut query = alarms::Entity::find()
            .filter(alarms::Column::ProjectId.eq(request.project_id))
            .filter(alarms::Column::AlarmType.eq(request.alarm_type.as_str()))
            .filter(alarms::Column::FiredAt.gte(cutoff));

        match request.deployment_id {
            Some(deployment_id) => {
                query = query.filter(alarms::Column::DeploymentId.eq(deployment_id));
            }
            None => {
                query = query.filter(alarms::Column::DeploymentId.is_null());
            }
        }

        match request.container_id {
            Some(container_id) => {
                query = query.filter(alarms::Column::ContainerId.eq(container_id));
            }
            None => {
                // Explicitly match NULL so that container-scoped alarms don't
                // suppress deployment-scoped alarms and vice versa.
                query = query.filter(alarms::Column::ContainerId.is_null());
            }
        }

        match request.service_id {
            Some(service_id) => {
                // Scope the cooldown to this service so a database metric alarm
                // on (say) Redis does not suppress an identically-typed alarm on
                // Postgres in the same project. Service-scoped alarms share
                // alarm_type `database_metric_threshold` and have NULL
                // deployment/container, so without this every service in a
                // project would collapse into a single cooldown bucket.
                query = query.filter(alarms::Column::ServiceId.eq(service_id));
            }
            None => {
                query = query.filter(alarms::Column::ServiceId.is_null());
            }
        }

        let count = query.count(self.db.as_ref()).await?;
        Ok(count > 0)
    }

    /// Build the human-readable metadata shown in the alarm notification's
    /// "Details" block.
    ///
    /// The notification renderer simply title-cases each metadata key and prints
    /// the value, so we resolve internal numeric IDs to the names/slugs an
    /// operator actually recognises (project slug, service name, environment
    /// slug) and deliberately omit the raw IDs — they are not actionable in an
    /// email. Each lookup degrades gracefully: if a name can't be resolved we
    /// skip that row rather than fall back to a bare ID.
    async fn build_notification_metadata(
        &self,
        project_id: i32,
        service_id: Option<i32>,
        environment_id: Option<i32>,
    ) -> HashMap<String, String> {
        let mut metadata: HashMap<String, String> = HashMap::new();

        // Project → slug (preferred) so the reader sees "my-app", not "4".
        if let Ok(Some(project)) = temps_entities::projects::Entity::find_by_id(project_id)
            .one(self.db.as_ref())
            .await
        {
            metadata.insert("project".to_string(), project.slug);
        }

        // Service → "name (type)" e.g. "cache (redis)" so the operator knows
        // exactly which database triggered the alarm.
        if let Some(service_id) = service_id {
            if let Ok(Some(service)) =
                temps_entities::external_services::Entity::find_by_id(service_id)
                    .one(self.db.as_ref())
                    .await
            {
                metadata.insert(
                    "service".to_string(),
                    format!("{} ({})", service.name, service.service_type),
                );
            }
        }

        // Environment → slug, only for deployment-scoped alarms that have one.
        if let Some(env_id) = environment_id {
            if let Ok(Some(env)) = temps_entities::environments::Entity::find_by_id(env_id)
                .one(self.db.as_ref())
                .await
            {
                metadata.insert("environment".to_string(), env.slug);
            }
        }

        metadata
    }

    /// Send notification for a fired alarm (failure is logged, not propagated)
    async fn send_alarm_notification(&self, request: &FireAlarmRequest, alarm_id: i32) {
        let mut metadata = self
            .build_notification_metadata(
                request.project_id,
                request.service_id,
                request.environment_id,
            )
            .await;
        // Merge caller-supplied detail (e.g. the OTel metric-alert's metric/value
        // fields and the reserved `_chart_svg` chart) so the channels can render
        // it — the email surfaces the chart, Slack/webhook skip `_`-prefixed keys.
        if let Some(serde_json::Value::Object(extra)) = &request.metadata {
            for (k, v) in extra {
                let s = match v {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                metadata.insert(k.clone(), s);
            }
        }

        let notification = NotificationData {
            id: uuid::Uuid::new_v4().to_string(),
            title: request.title.clone(),
            message: request.message.clone(),
            notification_type: request.severity.to_notification_type(),
            priority: request.severity.to_notification_priority(),
            severity: Some(request.severity.as_str().to_string()),
            timestamp: Utc::now(),
            metadata,
            bypass_throttling: request.severity == AlarmSeverity::Critical,
        };

        if let Err(e) = self
            .notification_service
            .send_notification(notification)
            .await
        {
            error!(
                "Failed to send alarm notification for alarm {}: {}",
                alarm_id, e
            );
        }
    }

    /// Send one ad-hoc digest notification not tied to a single alarm row.
    ///
    /// Used to collapse a burst of alarms (e.g. a per-series cardinality spike
    /// where every series still gets its own alarm via [`Self::fire_alarm_silent`])
    /// into ONE combined notification. Builds the same project-context metadata as
    /// [`Self::send_alarm_notification`] (via [`Self::build_notification_metadata`],
    /// no service/environment scope) and merges caller-supplied `metadata` on top,
    /// applies the same `Critical`-bypasses-throttling rule, and best-effort sends:
    /// a failure is logged, never propagated, so it cannot fail the caller.
    pub async fn send_digest_notification(
        &self,
        project_id: i32,
        severity: AlarmSeverity,
        title: String,
        message: String,
        metadata: Option<serde_json::Value>,
    ) {
        let mut notification_metadata = self
            .build_notification_metadata(project_id, None, None)
            .await;
        // Merge caller-supplied detail (e.g. the digest's fired-series list) so the
        // channels can render it — same mapping as `send_alarm_notification`.
        if let Some(serde_json::Value::Object(extra)) = &metadata {
            for (k, v) in extra {
                let s = match v {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                notification_metadata.insert(k.clone(), s);
            }
        }

        let notification = NotificationData {
            id: uuid::Uuid::new_v4().to_string(),
            title,
            message,
            notification_type: severity.to_notification_type(),
            priority: severity.to_notification_priority(),
            severity: Some(severity.as_str().to_string()),
            timestamp: Utc::now(),
            metadata: notification_metadata,
            bypass_throttling: severity == AlarmSeverity::Critical,
        };

        if let Err(e) = self
            .notification_service
            .send_notification(notification)
            .await
        {
            error!(
                "Failed to send digest notification for project {}: {}",
                project_id, e
            );
        }
    }

    /// Send recovery notification when an alarm resolves (failure is logged, not propagated)
    async fn send_resolved_notification(&self, alarm: &alarms::Model) {
        let mut metadata = self
            .build_notification_metadata(alarm.project_id, alarm.service_id, alarm.environment_id)
            .await;
        metadata.insert("status".to_string(), "resolved".to_string());

        let notification = NotificationData {
            id: uuid::Uuid::new_v4().to_string(),
            title: format!("Resolved: {}", alarm.title),
            message: format!(
                "Alarm '{}' has been resolved.\nOriginal severity: {}",
                alarm.title, alarm.severity
            ),
            notification_type: NotificationType::Info,
            priority: NotificationPriority::Normal,
            severity: None,
            timestamp: Utc::now(),
            metadata,
            bypass_throttling: false,
        };

        if let Err(e) = self
            .notification_service
            .send_notification(notification)
            .await
        {
            error!(
                "Failed to send resolved notification for alarm {}: {}",
                alarm.id, e
            );
        }
    }
}

/// Filters for listing alarms
#[derive(Debug, Default)]
pub struct AlarmFilters {
    pub environment_id: Option<i32>,
    pub deployment_id: Option<i32>,
    pub alarm_type: Option<AlarmType>,
    pub status: Option<AlarmStatus>,
    pub severity: Option<AlarmSeverity>,
}

/// Summary counts for dashboard widget
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct AlarmSummary {
    pub total_active: u32,
    pub firing: u32,
    pub acknowledged: u32,
    pub critical: u32,
    pub warning: u32,
    pub by_type: HashMap<String, u32>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};
    use std::sync::atomic::{AtomicU32, Ordering};
    use temps_core::jobs::QueueError;
    use temps_core::notifications::{EmailMessage, NotificationError};
    use temps_entities::external_services;

    // ── Test helpers ──────────────────────────────────────────────────

    /// Tracks how many notifications were sent and retains the most recent one
    /// so tests can assert on the rendered metadata (project slug, service name).
    struct TrackingNotificationService {
        send_count: AtomicU32,
        last_notification: std::sync::Mutex<Option<NotificationData>>,
    }

    impl TrackingNotificationService {
        fn new() -> Self {
            Self {
                send_count: AtomicU32::new(0),
                last_notification: std::sync::Mutex::new(None),
            }
        }

        fn send_count(&self) -> u32 {
            self.send_count.load(Ordering::SeqCst)
        }

        fn last_metadata(&self) -> HashMap<String, String> {
            self.last_notification
                .lock()
                .expect("notification mutex poisoned")
                .as_ref()
                .map(|n| n.metadata.clone())
                .unwrap_or_default()
        }
    }

    #[async_trait]
    impl NotificationService for TrackingNotificationService {
        async fn send_notification(
            &self,
            notification: NotificationData,
        ) -> Result<(), NotificationError> {
            self.send_count.fetch_add(1, Ordering::SeqCst);
            *self
                .last_notification
                .lock()
                .expect("notification mutex poisoned") = Some(notification);
            Ok(())
        }
        async fn send_email(&self, _message: EmailMessage) -> Result<(), NotificationError> {
            Ok(())
        }
        async fn is_configured(&self) -> Result<bool, NotificationError> {
            Ok(true)
        }
    }

    /// Tracks how many jobs were sent to the queue
    struct TrackingJobQueue {
        send_count: AtomicU32,
    }

    impl TrackingJobQueue {
        fn new() -> Self {
            Self {
                send_count: AtomicU32::new(0),
            }
        }

        fn send_count(&self) -> u32 {
            self.send_count.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl temps_core::JobQueue for TrackingJobQueue {
        async fn send(&self, _job: Job) -> Result<(), QueueError> {
            self.send_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn subscribe(&self) -> Box<dyn temps_core::JobReceiver> {
            unimplemented!("not needed in tests")
        }
    }

    fn sample_alarm(id: i32, alarm_type: &str, severity: &str, status: &str) -> alarms::Model {
        alarms::Model {
            id,
            project_id: 1,
            environment_id: Some(1),
            deployment_id: Some(10),
            container_id: Some(100),
            service_id: None,
            alarm_type: alarm_type.to_string(),
            severity: severity.to_string(),
            status: status.to_string(),
            title: format!("Test alarm {}", id),
            message: Some("Test message".to_string()),
            metadata: None,
            fired_at: Utc::now(),
            acknowledged_at: None,
            acknowledged_by: None,
            resolved_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn sample_fire_request() -> FireAlarmRequest {
        FireAlarmRequest {
            project_id: 1,
            environment_id: Some(1),
            deployment_id: Some(10),
            container_id: Some(100),
            service_id: None,
            alarm_type: AlarmType::ContainerRestart,
            severity: AlarmSeverity::Warning,
            title: "Container restarted".to_string(),
            message: "Container xyz restarted 3 times".to_string(),
            metadata: Some(serde_json::json!({"restart_count": 3})),
        }
    }

    // ── Type conversion tests ─────────────────────────────────────────

    #[test]
    fn test_alarm_type_roundtrip() {
        let types = [
            AlarmType::ContainerRestart,
            AlarmType::ContainerOomKilled,
            AlarmType::HighResponseTime,
            AlarmType::Outage,
            AlarmType::HighCpu,
            AlarmType::HighMemory,
            AlarmType::DeploymentFailed,
            AlarmType::HealthCheckFailed,
        ];

        for t in &types {
            let s = t.as_str();
            let parsed = AlarmType::parse_alarm_type(s);
            assert_eq!(parsed, Some(*t), "Roundtrip failed for {}", s);
        }
    }

    #[test]
    fn test_alarm_type_from_str_unknown() {
        assert_eq!(AlarmType::parse_alarm_type("unknown_type"), None);
        assert_eq!(AlarmType::parse_alarm_type(""), None);
    }

    #[test]
    fn test_alarm_severity_as_str() {
        assert_eq!(AlarmSeverity::Info.as_str(), "info");
        assert_eq!(AlarmSeverity::Warning.as_str(), "warning");
        assert_eq!(AlarmSeverity::Critical.as_str(), "critical");
    }

    #[test]
    fn test_alarm_severity_notification_priority() {
        assert!(matches!(
            AlarmSeverity::Info.to_notification_priority(),
            NotificationPriority::Low
        ));
        assert!(matches!(
            AlarmSeverity::Warning.to_notification_priority(),
            NotificationPriority::High
        ));
        assert!(matches!(
            AlarmSeverity::Critical.to_notification_priority(),
            NotificationPriority::Critical
        ));
    }

    #[test]
    fn test_alarm_severity_notification_type() {
        assert!(matches!(
            AlarmSeverity::Info.to_notification_type(),
            NotificationType::Info
        ));
        assert!(matches!(
            AlarmSeverity::Warning.to_notification_type(),
            NotificationType::Warning
        ));
        assert!(matches!(
            AlarmSeverity::Critical.to_notification_type(),
            NotificationType::Error
        ));
    }

    #[test]
    fn test_alarm_status_as_str() {
        assert_eq!(AlarmStatus::Firing.as_str(), "firing");
        assert_eq!(AlarmStatus::Acknowledged.as_str(), "acknowledged");
        assert_eq!(AlarmStatus::Resolved.as_str(), "resolved");
    }

    #[test]
    fn test_alarm_summary_default() {
        let summary = AlarmSummary::default();
        assert_eq!(summary.total_active, 0);
        assert_eq!(summary.firing, 0);
        assert_eq!(summary.acknowledged, 0);
        assert_eq!(summary.critical, 0);
        assert_eq!(summary.warning, 0);
        assert!(summary.by_type.is_empty());
    }

    #[test]
    fn test_alarm_error_display() {
        let not_found = AlarmError::NotFound {
            alarm_id: 42,
            project_id: 7,
        };
        assert_eq!(not_found.to_string(), "Alarm 42 not found in project 7");

        let db_err = AlarmError::Database {
            operation: "insert alarm".to_string(),
            reason: "connection refused".to_string(),
        };
        assert_eq!(
            db_err.to_string(),
            "Database error while insert alarm: connection refused"
        );

        let notif_err = AlarmError::Notification {
            alarm_id: 1,
            reason: "timeout".to_string(),
        };
        assert_eq!(
            notif_err.to_string(),
            "Notification error for alarm 1: timeout"
        );

        let queue_err = AlarmError::Queue {
            alarm_id: 2,
            reason: "channel closed".to_string(),
        };
        assert_eq!(
            queue_err.to_string(),
            "Queue error for alarm 2: channel closed"
        );
    }

    #[test]
    fn test_alarm_error_from_db_err() {
        let db_err = sea_orm::DbErr::RecordNotFound("alarms".to_string());
        let alarm_err: AlarmError = db_err.into();
        assert!(matches!(alarm_err, AlarmError::Database { .. }));
    }

    // ── AlarmService.fire_alarm tests ─────────────────────────────────

    #[tokio::test]
    async fn test_fire_alarm_success() {
        let alarm = sample_alarm(1, "container_restart", "warning", "firing");

        // Mock: cooldown check (count query returns 0) + insert
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([[maplit::btreemap! {
                "num_items" => sea_orm::Value::BigInt(Some(0)),
            }]])
            .append_query_results(vec![vec![alarm.clone()]])
            .into_connection();

        let notification_service = Arc::new(TrackingNotificationService::new());
        let job_queue = Arc::new(TrackingJobQueue::new());

        let service = AlarmService::new(
            Arc::new(db),
            notification_service.clone(),
            job_queue.clone(),
        );

        let result = service.fire_alarm(sample_fire_request()).await;
        assert!(result.is_ok());
        let alarm_id = result.unwrap();
        assert_eq!(alarm_id, Some(1));

        // Notification and job should be sent
        assert_eq!(notification_service.send_count(), 1);
        assert_eq!(job_queue.send_count(), 1);
    }

    #[tokio::test]
    async fn test_fire_alarm_suppressed_by_cooldown() {
        // Mock: cooldown check returns count > 0
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([[maplit::btreemap! {
                "num_items" => sea_orm::Value::BigInt(Some(1)),
            }]])
            .into_connection();

        let notification_service = Arc::new(TrackingNotificationService::new());
        let job_queue = Arc::new(TrackingJobQueue::new());

        let service = AlarmService::new(
            Arc::new(db),
            notification_service.clone(),
            job_queue.clone(),
        );

        let result = service.fire_alarm(sample_fire_request()).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), None, "Should be suppressed by cooldown");

        // No notification or job sent when suppressed
        assert_eq!(notification_service.send_count(), 0);
        assert_eq!(job_queue.send_count(), 0);
    }

    /// Minimal `external_services` row for notification-metadata resolution.
    fn sample_service(id: i32, name: &str, service_type: &str) -> external_services::Model {
        external_services::Model {
            id,
            name: name.to_string(),
            service_type: service_type.to_string(),
            version: None,
            status: "running".to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            slug: Some(name.to_string()),
            config: None,
            node_id: None,
            topology: "standalone".to_string(),
            error_message: None,
            health_status: None,
            last_health_check_at: None,
            last_health_error: None,
            consecutive_health_failures: 0,
            health_metadata: None,
            metrics_enabled: true,
            default_backup_provisioned: false,
            container_name: None,
        }
    }

    /// A database-metric (service-scoped) fire request: no environment or
    /// deployment, but a `service_id`. Mirrors what `AlertEvaluator` builds for
    /// a Redis/Postgres threshold rule.
    fn service_scoped_fire_request(service_id: i32) -> FireAlarmRequest {
        FireAlarmRequest {
            project_id: 4,
            environment_id: None,
            deployment_id: None,
            container_id: None,
            service_id: Some(service_id),
            alarm_type: AlarmType::DatabaseMetricThreshold,
            severity: AlarmSeverity::Warning,
            title: "Metric threshold breached: High memory fragmentation ratio".to_string(),
            message: "redis.memory_fragmentation_ratio is 7.600 (threshold: > 1.500)".to_string(),
            metadata: None,
        }
    }

    #[tokio::test]
    async fn test_service_alarm_notification_uses_names_not_ids() {
        // The alarm row inserted on fire — service-scoped, so no env/deployment.
        let mut alarm = sample_alarm(1, "database_metric_threshold", "warning", "firing");
        alarm.environment_id = None;
        alarm.deployment_id = None;
        alarm.container_id = None;
        alarm.service_id = Some(7);

        // Query order for a service-scoped fire_alarm:
        //   1. cooldown count (0) → not suppressed
        //   2. insert alarm
        //   3. project lookup (build_notification_metadata) — empty: graceful skip
        //   4. service lookup → returns the redis service
        // (no environment lookup: environment_id is None)
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([[maplit::btreemap! {
                "num_items" => sea_orm::Value::BigInt(Some(0)),
            }]])
            .append_query_results(vec![vec![alarm]])
            .append_query_results(vec![Vec::<temps_entities::projects::Model>::new()])
            .append_query_results(vec![vec![sample_service(7, "cache", "redis")]])
            .into_connection();

        let notification_service = Arc::new(TrackingNotificationService::new());
        let job_queue = Arc::new(TrackingJobQueue::new());

        let service = AlarmService::new(
            Arc::new(db),
            notification_service.clone(),
            job_queue.clone(),
        );

        let result = service.fire_alarm(service_scoped_fire_request(7)).await;
        assert_eq!(result.unwrap(), Some(1));
        assert_eq!(notification_service.send_count(), 1);

        let metadata = notification_service.last_metadata();

        // The service is resolved to a human-readable "name (type)" so the
        // operator sees WHICH database breached, not just a project number.
        assert_eq!(
            metadata.get("service").map(String::as_str),
            Some("cache (redis)"),
            "notification should name the service: {metadata:?}"
        );

        // Raw numeric IDs must NOT leak into the email DETAILS block.
        for forbidden in ["project_id", "service_id", "alarm_id", "deployment_id"] {
            assert!(
                !metadata.contains_key(forbidden),
                "metadata must not contain raw id key '{forbidden}': {metadata:?}"
            );
        }
    }

    // ── AlarmService.fire_alarm_silent / send_digest_notification ─────

    #[tokio::test]
    async fn test_fire_alarm_silent_persists_alarm_and_job_but_no_notification() {
        let alarm = sample_alarm(1, "deployment_metric_threshold", "warning", "firing");

        // Mock: cooldown check (0) + insert. No project/service lookups occur
        // because the individual notification (which would resolve them) is
        // suppressed by the silent path.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([[maplit::btreemap! {
                "num_items" => sea_orm::Value::BigInt(Some(0)),
            }]])
            .append_query_results(vec![vec![alarm]])
            .into_connection();

        let notification_service = Arc::new(TrackingNotificationService::new());
        let job_queue = Arc::new(TrackingJobQueue::new());

        let service = AlarmService::new(
            Arc::new(db),
            notification_service.clone(),
            job_queue.clone(),
        );

        let result = service.fire_alarm_silent(sample_fire_request()).await;
        assert_eq!(result.unwrap(), Some(1));

        // The alarm row + AlarmFired job still happen (dashboards/webhooks react),
        // but NO individual human notification is sent.
        assert_eq!(
            notification_service.send_count(),
            0,
            "silent fire must not notify"
        );
        assert_eq!(job_queue.send_count(), 1, "AlarmFired job still emitted");
    }

    #[tokio::test]
    async fn test_send_digest_notification_sends_single_merged_notification() {
        // build_notification_metadata does one project lookup; an empty result
        // means it gracefully skips the "project" row. The caller-supplied digest
        // metadata is then merged on top and stringified.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<temps_entities::projects::Model>::new()])
            .into_connection();

        let notification_service = Arc::new(TrackingNotificationService::new());
        let job_queue = Arc::new(TrackingJobQueue::new());

        let service = AlarmService::new(
            Arc::new(db),
            notification_service.clone(),
            job_queue.clone(),
        );

        service
            .send_digest_notification(
                4,
                AlarmSeverity::Warning,
                "12 series of http.latency breached".to_string(),
                "endpoint=/checkout (500)\nand 2 more".to_string(),
                Some(serde_json::json!({
                    "rule_id": 9,
                    "source": "otel_metric_alert_digest",
                })),
            )
            .await;

        // Exactly one notification; no AlarmFired job (a digest isn't one alarm).
        assert_eq!(notification_service.send_count(), 1);
        assert_eq!(job_queue.send_count(), 0);

        let metadata = notification_service.last_metadata();
        assert_eq!(
            metadata.get("source").map(String::as_str),
            Some("otel_metric_alert_digest"),
            "digest metadata must be merged into the notification: {metadata:?}"
        );
        assert_eq!(metadata.get("rule_id").map(String::as_str), Some("9"));
    }

    // ── AlarmService.resolve_alarm tests ──────────────────────────────

    #[tokio::test]
    async fn test_resolve_alarm_success() {
        let alarm = sample_alarm(1, "outage", "critical", "firing");

        // Mock: find alarm + update
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![alarm.clone()]])
            .append_query_results(vec![vec![alarm.clone()]])
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 1,
                rows_affected: 1,
            }])
            .into_connection();

        let notification_service = Arc::new(TrackingNotificationService::new());
        let job_queue = Arc::new(TrackingJobQueue::new());

        let service = AlarmService::new(
            Arc::new(db),
            notification_service.clone(),
            job_queue.clone(),
        );

        let result = service.resolve_alarm(1, 1).await;
        assert!(result.is_ok());

        // Should send resolved notification + AlarmResolved job
        assert_eq!(notification_service.send_count(), 1);
        assert_eq!(job_queue.send_count(), 1);
    }

    #[tokio::test]
    async fn test_resolve_alarm_not_found() {
        // Mock: find returns empty
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<alarms::Model>::new()])
            .into_connection();

        let notification_service = Arc::new(TrackingNotificationService::new());
        let job_queue = Arc::new(TrackingJobQueue::new());

        let service = AlarmService::new(
            Arc::new(db),
            notification_service.clone(),
            job_queue.clone(),
        );

        let result = service.resolve_alarm(999, 1).await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            AlarmError::NotFound {
                alarm_id: 999,
                project_id: 1
            }
        ));

        // No notification or job sent
        assert_eq!(notification_service.send_count(), 0);
        assert_eq!(job_queue.send_count(), 0);
    }

    #[tokio::test]
    async fn test_resolve_alarm_already_resolved_is_noop() {
        let alarm = sample_alarm(1, "outage", "critical", "resolved");

        // Mock: find returns already-resolved alarm
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![alarm]])
            .into_connection();

        let notification_service = Arc::new(TrackingNotificationService::new());
        let job_queue = Arc::new(TrackingJobQueue::new());

        let service = AlarmService::new(
            Arc::new(db),
            notification_service.clone(),
            job_queue.clone(),
        );

        let result = service.resolve_alarm(1, 1).await;
        assert!(result.is_ok());

        // No notification or job: it was already resolved
        assert_eq!(notification_service.send_count(), 0);
        assert_eq!(job_queue.send_count(), 0);
    }

    // ── AlarmService.resolve_alarms_by_type tests ─────────────────────

    #[tokio::test]
    async fn test_resolve_alarms_by_type_resolves_all_firing() {
        let alarm1 = sample_alarm(1, "outage", "critical", "firing");
        let alarm2 = sample_alarm(2, "outage", "warning", "firing");

        // Mock:
        // 1. SELECT to find firing alarms of type outage
        // 2. batch UPDATE exec (inside transaction begin/commit)
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![alarm1.clone(), alarm2.clone()]])
            // batch UPDATE inside transaction
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 2,
            }])
            .into_connection();

        let notification_service = Arc::new(TrackingNotificationService::new());
        let job_queue = Arc::new(TrackingJobQueue::new());

        let service = AlarmService::new(
            Arc::new(db),
            notification_service.clone(),
            job_queue.clone(),
        );

        let result = service
            .resolve_alarms_by_type(1, 10, AlarmType::Outage)
            .await;
        assert!(result.is_ok(), "expected Ok, got {:?}", result.err());
        let resolved_ids = result.unwrap();
        assert_eq!(resolved_ids.len(), 2);
        assert!(resolved_ids.contains(&1));
        assert!(resolved_ids.contains(&2));

        // 2 notifications + 2 jobs (one per resolved alarm)
        assert_eq!(notification_service.send_count(), 2);
        assert_eq!(job_queue.send_count(), 2);
    }

    #[tokio::test]
    async fn test_resolve_alarms_by_type_none_firing() {
        // Mock: find returns no firing alarms — no transaction or exec needed
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<alarms::Model>::new()])
            .into_connection();

        let notification_service = Arc::new(TrackingNotificationService::new());
        let job_queue = Arc::new(TrackingJobQueue::new());

        let service = AlarmService::new(
            Arc::new(db),
            notification_service.clone(),
            job_queue.clone(),
        );

        let result = service
            .resolve_alarms_by_type(1, 10, AlarmType::Outage)
            .await;
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());

        assert_eq!(notification_service.send_count(), 0);
        assert_eq!(job_queue.send_count(), 0);
    }

    // ── AlarmService.acknowledge_alarm tests ──────────────────────────

    #[tokio::test]
    async fn test_acknowledge_alarm_success() {
        let alarm = sample_alarm(1, "high_cpu", "warning", "firing");

        // Mock: find + update
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![alarm.clone()]])
            .append_query_results(vec![vec![alarm.clone()]])
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 1,
                rows_affected: 1,
            }])
            .into_connection();

        let notification_service = Arc::new(TrackingNotificationService::new());
        let job_queue = Arc::new(TrackingJobQueue::new());

        let service = AlarmService::new(Arc::new(db), notification_service, job_queue);

        let result = service.acknowledge_alarm(1, 1, 42).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_acknowledge_alarm_not_found() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<alarms::Model>::new()])
            .into_connection();

        let notification_service = Arc::new(TrackingNotificationService::new());
        let job_queue = Arc::new(TrackingJobQueue::new());

        let service = AlarmService::new(Arc::new(db), notification_service, job_queue);

        let result = service.acknowledge_alarm(999, 1, 42).await;
        assert!(matches!(
            result.unwrap_err(),
            AlarmError::NotFound {
                alarm_id: 999,
                project_id: 1
            }
        ));
    }

    #[tokio::test]
    async fn test_acknowledge_already_resolved_is_noop() {
        let alarm = sample_alarm(1, "high_cpu", "warning", "resolved");

        // Mock: find only (no update since it's already resolved)
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![alarm]])
            .into_connection();

        let notification_service = Arc::new(TrackingNotificationService::new());
        let job_queue = Arc::new(TrackingJobQueue::new());

        let service = AlarmService::new(Arc::new(db), notification_service, job_queue);

        let result = service.acknowledge_alarm(1, 1, 42).await;
        assert!(result.is_ok()); // noop, no error
    }

    // ── AlarmService.get_alarm_summary tests ──────────────────────────
    // NOTE: get_alarm_summary now uses two raw SQL aggregation queries rather
    // than loading all alarm rows. We mock both query result sets with btreemap
    // rows matching the SELECT columns (status, severity, cnt) and (alarm_type, cnt).

    #[tokio::test]
    async fn test_get_alarm_summary_mixed() {
        // First query: GROUP BY status, severity
        // Data: 3 firing/warning, 2 firing/critical, 1 acknowledged/warning
        // (mimics: container_restart=warning/firing, outage×2=critical/firing,
        //          high_cpu=warning/acknowledged)
        let status_severity_rows = vec![
            maplit::btreemap! {
                "status"   => sea_orm::Value::String(Some(Box::new("firing".to_string()))),
                "severity" => sea_orm::Value::String(Some(Box::new("warning".to_string()))),
                "cnt"      => sea_orm::Value::BigInt(Some(1)),
            },
            maplit::btreemap! {
                "status"   => sea_orm::Value::String(Some(Box::new("firing".to_string()))),
                "severity" => sea_orm::Value::String(Some(Box::new("critical".to_string()))),
                "cnt"      => sea_orm::Value::BigInt(Some(2)),
            },
            maplit::btreemap! {
                "status"   => sea_orm::Value::String(Some(Box::new("acknowledged".to_string()))),
                "severity" => sea_orm::Value::String(Some(Box::new("warning".to_string()))),
                "cnt"      => sea_orm::Value::BigInt(Some(1)),
            },
        ];

        // Second query: GROUP BY alarm_type
        let alarm_type_rows = vec![
            maplit::btreemap! {
                "alarm_type" => sea_orm::Value::String(Some(Box::new("outage".to_string()))),
                "cnt"        => sea_orm::Value::BigInt(Some(2)),
            },
            maplit::btreemap! {
                "alarm_type" => sea_orm::Value::String(Some(Box::new("container_restart".to_string()))),
                "cnt"        => sea_orm::Value::BigInt(Some(1)),
            },
            maplit::btreemap! {
                "alarm_type" => sea_orm::Value::String(Some(Box::new("high_cpu".to_string()))),
                "cnt"        => sea_orm::Value::BigInt(Some(1)),
            },
        ];

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![status_severity_rows])
            .append_query_results(vec![alarm_type_rows])
            .into_connection();

        let notification_service = Arc::new(TrackingNotificationService::new());
        let job_queue = Arc::new(TrackingJobQueue::new());

        let service = AlarmService::new(Arc::new(db), notification_service, job_queue);

        let summary = service.get_alarm_summary(1).await.unwrap();
        assert_eq!(summary.firing, 3); // 1 warning + 2 critical
        assert_eq!(summary.acknowledged, 1);
        assert_eq!(summary.total_active, 4);
        assert_eq!(summary.critical, 2);
        assert_eq!(summary.warning, 2); // 1 firing/warning + 1 acknowledged/warning
        assert_eq!(summary.by_type.get("outage"), Some(&2));
        assert_eq!(summary.by_type.get("container_restart"), Some(&1));
        assert_eq!(summary.by_type.get("high_cpu"), Some(&1));
    }

    #[tokio::test]
    async fn test_get_alarm_summary_empty() {
        // Both queries return empty result sets.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![
                Vec::<std::collections::BTreeMap<&str, sea_orm::Value>>::new(),
            ])
            .append_query_results(vec![
                Vec::<std::collections::BTreeMap<&str, sea_orm::Value>>::new(),
            ])
            .into_connection();

        let notification_service = Arc::new(TrackingNotificationService::new());
        let job_queue = Arc::new(TrackingJobQueue::new());

        let service = AlarmService::new(Arc::new(db), notification_service, job_queue);

        let summary = service.get_alarm_summary(1).await.unwrap();
        assert_eq!(summary.total_active, 0);
        assert_eq!(summary.firing, 0);
        assert_eq!(summary.acknowledged, 0);
        assert!(summary.by_type.is_empty());
    }

    // ── AlarmService.list_alarms tests ────────────────────────────────

    #[tokio::test]
    async fn test_list_alarms_paginated() {
        let alarms = vec![
            sample_alarm(2, "outage", "critical", "firing"),
            sample_alarm(1, "container_restart", "warning", "firing"),
        ];

        // Mock: count query + fetch page
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([[maplit::btreemap! {
                "num_items" => sea_orm::Value::BigInt(Some(5)),
            }]])
            .append_query_results(vec![alarms])
            .into_connection();

        let notification_service = Arc::new(TrackingNotificationService::new());
        let job_queue = Arc::new(TrackingJobQueue::new());

        let service = AlarmService::new(Arc::new(db), notification_service, job_queue);

        let (items, total) = service
            .list_alarms(1, AlarmFilters::default(), 1, 20)
            .await
            .unwrap();

        assert_eq!(total, 5);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].id, 2);
        assert_eq!(items[1].id, 1);
    }

    #[tokio::test]
    async fn test_list_alarms_page_size_capped_at_100() {
        // Even if we request 500, it should be capped at 100
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([[maplit::btreemap! {
                "num_items" => sea_orm::Value::BigInt(Some(0)),
            }]])
            .append_query_results(vec![Vec::<alarms::Model>::new()])
            .into_connection();

        let notification_service = Arc::new(TrackingNotificationService::new());
        let job_queue = Arc::new(TrackingJobQueue::new());

        let service = AlarmService::new(Arc::new(db), notification_service, job_queue);

        let result = service
            .list_alarms(1, AlarmFilters::default(), 1, 500)
            .await;
        assert!(result.is_ok());
    }

    // ── Cooldown configuration tests ──────────────────────────────────

    #[test]
    fn test_with_cooldown_configures_duration() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let notification_service = Arc::new(TrackingNotificationService::new());
        let job_queue = Arc::new(TrackingJobQueue::new());

        let service = AlarmService::new(Arc::new(db), notification_service, job_queue)
            .with_cooldown(chrono::Duration::minutes(15));

        assert_eq!(service.cooldown, chrono::Duration::minutes(15));
    }

    // ── FireAlarmRequest construction tests ───────────────────────────

    #[test]
    fn test_fire_alarm_request_metadata_serialization() {
        let request = sample_fire_request();
        assert!(request.metadata.is_some());
        let meta = request.metadata.unwrap();
        assert_eq!(meta["restart_count"], 3);
    }

    // ── Critical severity bypasses notification throttling ────────────

    #[tokio::test]
    async fn test_fire_critical_alarm_bypasses_throttling() {
        let alarm = sample_alarm(1, "container_oom_killed", "critical", "firing");

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([[maplit::btreemap! {
                "num_items" => sea_orm::Value::BigInt(Some(0)),
            }]])
            .append_query_results(vec![vec![alarm]])
            .into_connection();

        let notification_service = Arc::new(TrackingNotificationService::new());
        let job_queue = Arc::new(TrackingJobQueue::new());

        let service = AlarmService::new(Arc::new(db), notification_service.clone(), job_queue);

        let mut request = sample_fire_request();
        request.severity = AlarmSeverity::Critical;
        request.alarm_type = AlarmType::ContainerOomKilled;

        let result = service.fire_alarm(request).await;
        assert!(result.is_ok());
        assert_eq!(notification_service.send_count(), 1);
    }
}
