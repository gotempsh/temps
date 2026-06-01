//! Outage detection and notification service
//!
//! Monitors status checks for state transitions and sends alerts when outages occur.

use crate::alarm_service::{AlarmService, AlarmSeverity, AlarmType, FireAlarmRequest};
use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, Order, QueryFilter, QueryOrder,
    Set,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use temps_core::notifications::{
    NotificationData, NotificationPriority, NotificationService, NotificationType,
};
use temps_core::{AutopilotTriggerJob, Job, JobQueue, JobReceiver};
use temps_entities::{environments, status_checks, status_incidents, status_monitors};
use thiserror::Error;
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

/// Monitor status states
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MonitorStatus {
    Operational,
    Degraded,
    Down,
}

impl MonitorStatus {
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "operational" => Self::Operational,
            "degraded" | "partial_outage" => Self::Degraded,
            "down" | "major_outage" => Self::Down,
            _ => Self::Operational,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Operational => "operational",
            Self::Degraded => "degraded",
            Self::Down => "down",
        }
    }

    /// Check if this status represents an outage
    pub fn is_outage(&self) -> bool {
        matches!(self, Self::Degraded | Self::Down)
    }
}

/// Incident severity levels
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum IncidentSeverity {
    Minor,
    Major,
    Critical,
}

impl IncidentSeverity {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Minor => "minor",
            Self::Major => "major",
            Self::Critical => "critical",
        }
    }

    pub fn from_status(status: MonitorStatus) -> Self {
        match status {
            MonitorStatus::Degraded => Self::Minor,
            MonitorStatus::Down => Self::Major,
            MonitorStatus::Operational => Self::Minor,
        }
    }

    pub fn to_priority(&self) -> NotificationPriority {
        match self {
            Self::Minor => NotificationPriority::Normal,
            Self::Major => NotificationPriority::High,
            Self::Critical => NotificationPriority::Critical,
        }
    }
}

/// Cached monitor state
#[derive(Debug, Clone)]
struct MonitorState {
    status: MonitorStatus,
    active_incident_id: Option<i32>,
    consecutive_failures: u32,
}

/// Outage event for notification
#[derive(Debug, Clone, Serialize)]
pub struct OutageEvent {
    pub monitor_id: i32,
    pub monitor_name: String,
    pub project_id: i32,
    pub environment_id: Option<i32>,
    pub previous_status: MonitorStatus,
    pub current_status: MonitorStatus,
    pub error_message: Option<String>,
    pub incident_id: Option<i32>,
    pub occurred_at: DateTime<Utc>,
}

#[derive(Debug, Error)]
pub enum OutageError {
    #[error("Database error: {0}")]
    Database(String),
    #[error("Monitor not found: {0}")]
    MonitorNotFound(i32),
    #[error("Notification error: {0}")]
    Notification(String),
}

impl From<sea_orm::DbErr> for OutageError {
    fn from(err: sea_orm::DbErr) -> Self {
        OutageError::Database(err.to_string())
    }
}

/// Outage detection and notification service
pub struct OutageDetectionService {
    db: Arc<DatabaseConnection>,
    notification_service: Arc<dyn NotificationService>,
    alarm_service: Arc<AlarmService>,
    /// Optional job queue for emitting workflow triggers on outage events.
    /// When set, monitoring.downtime workflows will be fired automatically.
    job_queue: Option<Arc<dyn JobQueue>>,
    /// Cache of monitor states to detect transitions
    monitor_states: RwLock<HashMap<i32, MonitorState>>,
    /// Number of consecutive failures before triggering alert
    failure_threshold: u32,
    /// Minimum time between alerts for same monitor (to avoid spam)
    alert_cooldown: Duration,
}

impl OutageDetectionService {
    pub fn new(
        db: Arc<DatabaseConnection>,
        notification_service: Arc<dyn NotificationService>,
        alarm_service: Arc<AlarmService>,
    ) -> Self {
        Self {
            db,
            notification_service,
            alarm_service,
            job_queue: None,
            monitor_states: RwLock::new(HashMap::new()),
            failure_threshold: 1, // Alert on first failure
            alert_cooldown: Duration::minutes(5),
        }
    }

    /// Attach a job queue so this service can emit workflow trigger events
    /// when outages are detected.
    pub fn with_job_queue(mut self, queue: Arc<dyn JobQueue>) -> Self {
        self.job_queue = Some(queue);
        self
    }

    /// Configure the failure threshold
    pub fn with_failure_threshold(mut self, threshold: u32) -> Self {
        self.failure_threshold = threshold;
        self
    }

    /// Configure the alert cooldown
    pub fn with_alert_cooldown(mut self, cooldown: Duration) -> Self {
        self.alert_cooldown = cooldown;
        self
    }

    /// Process a new status check and detect state transitions
    pub async fn process_check(
        &self,
        monitor_id: i32,
        status: MonitorStatus,
        error_message: Option<String>,
    ) -> Result<Option<OutageEvent>, OutageError> {
        let now = Utc::now();

        // Get monitor details
        let monitor = status_monitors::Entity::find_by_id(monitor_id)
            .one(self.db.as_ref())
            .await?
            .ok_or(OutageError::MonitorNotFound(monitor_id))?;

        let mut states = self.monitor_states.write().await;
        let previous_state = states.get(&monitor_id).cloned();

        // Determine if this is a state transition
        let (_should_alert, event) = match &previous_state {
            Some(prev) => {
                let is_transition = prev.status != status;
                let consecutive_failures = if status.is_outage() {
                    prev.consecutive_failures + 1
                } else {
                    0
                };

                // Check if we should alert
                let should_alert = if is_transition {
                    if status.is_outage() {
                        // New outage: alert if we've hit threshold
                        consecutive_failures >= self.failure_threshold
                    } else {
                        // Recovery: always alert (monitor came back up)
                        prev.status.is_outage()
                    }
                } else {
                    false
                };

                let event = if should_alert {
                    Some(OutageEvent {
                        monitor_id,
                        monitor_name: monitor.name.clone(),
                        project_id: monitor.project_id,
                        environment_id: monitor.environment_id,
                        previous_status: prev.status,
                        current_status: status,
                        error_message: error_message.clone(),
                        incident_id: prev.active_incident_id,
                        occurred_at: now,
                    })
                } else {
                    None
                };

                // Update state
                states.insert(
                    monitor_id,
                    MonitorState {
                        status,
                        active_incident_id: if status.is_outage() {
                            prev.active_incident_id
                        } else {
                            None
                        },
                        consecutive_failures,
                    },
                );

                (should_alert, event)
            }
            None => {
                // First check for this monitor - initialize state
                let is_down = status.is_outage();
                states.insert(
                    monitor_id,
                    MonitorState {
                        status,
                        active_incident_id: None,
                        consecutive_failures: if is_down { 1 } else { 0 },
                    },
                );

                // Alert immediately if monitor is down on first check
                let event = if is_down {
                    Some(OutageEvent {
                        monitor_id,
                        monitor_name: monitor.name.clone(),
                        project_id: monitor.project_id,
                        environment_id: monitor.environment_id,
                        previous_status: MonitorStatus::Operational,
                        current_status: status,
                        error_message: error_message.clone(),
                        incident_id: None,
                        occurred_at: now,
                    })
                } else {
                    None
                };

                (is_down, event)
            }
        };

        drop(states);

        // Handle incident creation/resolution and notifications
        if let Some(ref event) = event {
            self.handle_outage_event(event).await?;
        }

        Ok(event)
    }

    /// Handle an outage event: create/resolve incidents, fire/resolve alarms, and send notifications
    async fn handle_outage_event(&self, event: &OutageEvent) -> Result<(), OutageError> {
        if event.current_status.is_outage() {
            // New outage - create incident
            let incident_id = self.create_incident(event).await?;
            self.send_outage_notification(event, incident_id).await?;

            // Fire alarm for the outage
            self.fire_outage_alarm(event).await;

            // Trigger any workflows configured to run on monitoring.downtime
            self.trigger_downtime_workflows(event).await;

            // Update cached state with incident ID
            let mut states = self.monitor_states.write().await;
            if let Some(state) = states.get_mut(&event.monitor_id) {
                state.active_incident_id = Some(incident_id);
            }
        } else {
            // Recovery - resolve incident
            if let Some(incident_id) = event.incident_id {
                self.resolve_incident(incident_id).await?;
            }
            self.send_recovery_notification(event).await?;

            // Resolve outage alarms for this deployment
            self.resolve_outage_alarm(event).await;
        }

        Ok(())
    }

    /// Emit an AutopilotTrigger job for monitoring.downtime workflows.
    /// Failures here are logged but never fail the parent operation —
    /// workflow triggering is best-effort.
    async fn trigger_downtime_workflows(&self, event: &OutageEvent) {
        let queue = match &self.job_queue {
            Some(q) => q,
            None => return,
        };

        let job = Job::AutopilotTrigger(AutopilotTriggerJob {
            project_id: event.project_id,
            trigger_type: "monitoring_downtime".to_string(),
            trigger_source_id: Some(event.monitor_id),
            trigger_source_type: Some("status_monitor".to_string()),
            error_group_id: None,
        });

        if let Err(e) = queue.send(job).await {
            warn!(
                "Failed to enqueue monitoring.downtime workflow trigger for monitor {}: {}",
                event.monitor_id, e
            );
        } else {
            info!(
                "Enqueued monitoring.downtime workflow trigger for project {} monitor {}",
                event.project_id, event.monitor_id
            );
        }
    }

    /// Fire an alarm when an outage is detected
    async fn fire_outage_alarm(&self, event: &OutageEvent) {
        // Look up the environment to get the current deployment_id
        let environment_id = match event.environment_id {
            Some(id) => id,
            None => {
                debug!(
                    "No environment_id on outage event for monitor {}, skipping alarm",
                    event.monitor_id
                );
                return;
            }
        };

        let environment = match environments::Entity::find_by_id(environment_id)
            .one(self.db.as_ref())
            .await
        {
            Ok(Some(env)) => env,
            Ok(None) => {
                debug!("Environment {} not found, skipping alarm", environment_id);
                return;
            }
            Err(e) => {
                error!(
                    "Failed to query environment {} for alarm: {}",
                    environment_id, e
                );
                return;
            }
        };

        let deployment_id = match environment.current_deployment_id {
            Some(id) => id,
            None => {
                debug!(
                    "Environment {} has no current deployment, skipping alarm",
                    environment_id
                );
                return;
            }
        };

        let severity = match event.current_status {
            MonitorStatus::Down => AlarmSeverity::Critical,
            MonitorStatus::Degraded => AlarmSeverity::Warning,
            MonitorStatus::Operational => return, // shouldn't happen
        };

        let request = FireAlarmRequest {
            project_id: event.project_id,
            environment_id: Some(environment_id),
            deployment_id: Some(deployment_id),
            container_id: None,
            service_id: None,
            alarm_type: AlarmType::Outage,
            severity,
            title: format!(
                "{} is {}",
                event.monitor_name,
                event.current_status.as_str()
            ),
            message: format!(
                "Monitor '{}' status changed from {} to {}.{}",
                event.monitor_name,
                event.previous_status.as_str(),
                event.current_status.as_str(),
                event
                    .error_message
                    .as_ref()
                    .map(|m| format!("\n\n{}", m))
                    .unwrap_or_default()
            ),
            metadata: Some(serde_json::json!({
                "monitor_id": event.monitor_id,
                "monitor_name": event.monitor_name,
                "previous_status": event.previous_status.as_str(),
                "current_status": event.current_status.as_str(),
            })),
        };

        if let Err(e) = self.alarm_service.fire_alarm(request).await {
            error!(
                "Failed to fire outage alarm for monitor {}: {}",
                event.monitor_id, e
            );
        }
    }

    /// Resolve outage alarms when the monitor recovers
    async fn resolve_outage_alarm(&self, event: &OutageEvent) {
        let environment_id = match event.environment_id {
            Some(id) => id,
            None => return,
        };

        let environment = match environments::Entity::find_by_id(environment_id)
            .one(self.db.as_ref())
            .await
        {
            Ok(Some(env)) => env,
            _ => return,
        };

        let deployment_id = match environment.current_deployment_id {
            Some(id) => id,
            None => return,
        };

        if let Err(e) = self
            .alarm_service
            .resolve_alarms_by_type(event.project_id, deployment_id, AlarmType::Outage)
            .await
        {
            error!(
                "Failed to resolve outage alarms for monitor {}: {}",
                event.monitor_id, e
            );
        }
    }

    /// Create a new incident for an outage
    async fn create_incident(&self, event: &OutageEvent) -> Result<i32, OutageError> {
        let severity = IncidentSeverity::from_status(event.current_status);

        let incident = status_incidents::ActiveModel {
            project_id: Set(event.project_id),
            environment_id: Set(event.environment_id),
            monitor_id: Set(Some(event.monitor_id)),
            title: Set(format!(
                "{} is {}",
                event.monitor_name,
                event.current_status.as_str()
            )),
            description: Set(event.error_message.clone()),
            severity: Set(severity.as_str().to_string()),
            status: Set("investigating".to_string()),
            started_at: Set(event.occurred_at),
            ..Default::default()
        };

        let result = incident.insert(self.db.as_ref()).await?;
        info!(
            "Created incident {} for monitor {} ({})",
            result.id, event.monitor_id, event.monitor_name
        );

        Ok(result.id)
    }

    /// Resolve an existing incident
    async fn resolve_incident(&self, incident_id: i32) -> Result<(), OutageError> {
        let incident = status_incidents::Entity::find_by_id(incident_id)
            .one(self.db.as_ref())
            .await?;

        if let Some(incident) = incident {
            let mut active: status_incidents::ActiveModel = incident.into();
            active.status = Set("resolved".to_string());
            active.resolved_at = Set(Some(Utc::now()));
            active.update(self.db.as_ref()).await?;
            info!("Resolved incident {}", incident_id);
        }

        Ok(())
    }

    /// Send notification for an outage
    async fn send_outage_notification(
        &self,
        event: &OutageEvent,
        incident_id: i32,
    ) -> Result<(), OutageError> {
        let severity = IncidentSeverity::from_status(event.current_status);

        let notification = NotificationData {
            id: uuid::Uuid::new_v4().to_string(),
            title: format!(
                "🚨 {} is {}",
                event.monitor_name,
                event.current_status.as_str()
            ),
            message: format!(
                "Monitor '{}' status changed from {} to {}.\n\n{}",
                event.monitor_name,
                event.previous_status.as_str(),
                event.current_status.as_str(),
                event
                    .error_message
                    .as_deref()
                    .unwrap_or("No error details available.")
            ),
            notification_type: if event.current_status == MonitorStatus::Down {
                NotificationType::Error
            } else {
                NotificationType::Warning
            },
            priority: severity.to_priority(),
            severity: Some(severity.as_str().to_string()),
            timestamp: event.occurred_at,
            metadata: [
                ("monitor_id".to_string(), event.monitor_id.to_string()),
                ("monitor_name".to_string(), event.monitor_name.clone()),
                ("project_id".to_string(), event.project_id.to_string()),
                ("incident_id".to_string(), incident_id.to_string()),
                (
                    "status".to_string(),
                    event.current_status.as_str().to_string(),
                ),
            ]
            .into_iter()
            .collect(),
            bypass_throttling: severity == IncidentSeverity::Critical,
        };

        self.notification_service
            .send_notification(notification)
            .await
            .map_err(|e| OutageError::Notification(e.to_string()))?;

        Ok(())
    }

    /// Send notification for a recovery
    async fn send_recovery_notification(&self, event: &OutageEvent) -> Result<(), OutageError> {
        let notification = NotificationData {
            id: uuid::Uuid::new_v4().to_string(),
            title: format!("✅ {} is back online", event.monitor_name),
            message: format!(
                "Monitor '{}' has recovered.\n\nPrevious status: {}\nCurrent status: {}",
                event.monitor_name,
                event.previous_status.as_str(),
                event.current_status.as_str()
            ),
            notification_type: NotificationType::Info,
            priority: NotificationPriority::Normal,
            severity: None,
            timestamp: event.occurred_at,
            metadata: [
                ("monitor_id".to_string(), event.monitor_id.to_string()),
                ("monitor_name".to_string(), event.monitor_name.clone()),
                ("project_id".to_string(), event.project_id.to_string()),
                ("status".to_string(), "recovered".to_string()),
            ]
            .into_iter()
            .collect(),
            bypass_throttling: false,
        };

        self.notification_service
            .send_notification(notification)
            .await
            .map_err(|e| OutageError::Notification(e.to_string()))?;

        Ok(())
    }

    /// Scan all active monitors and detect outages from recent checks.
    /// Also prunes cached state for monitors that are no longer active in the database,
    /// preventing unbounded growth of the in-memory state map.
    pub async fn scan_monitors(&self) -> Result<Vec<OutageEvent>, OutageError> {
        let monitors = status_monitors::Entity::find()
            .filter(status_monitors::Column::IsActive.eq(true))
            .all(self.db.as_ref())
            .await?;

        // Prune cached state for monitors that are no longer active
        let active_ids: std::collections::HashSet<i32> = monitors.iter().map(|m| m.id).collect();
        {
            let mut states = self.monitor_states.write().await;
            let before = states.len();
            states.retain(|id, _| active_ids.contains(id));
            let pruned = before - states.len();
            if pruned > 0 {
                debug!(
                    "Pruned {} stale monitor state entries ({} active)",
                    pruned,
                    states.len()
                );
            }
        }

        let mut events = Vec::new();

        for monitor in monitors {
            // Get the most recent check for this monitor
            let recent_check = status_checks::Entity::find()
                .filter(status_checks::Column::MonitorId.eq(monitor.id))
                .order_by(status_checks::Column::CheckedAt, Order::Desc)
                .one(self.db.as_ref())
                .await?;

            if let Some(check) = recent_check {
                let status = MonitorStatus::from_str(&check.status);
                if let Some(event) = self
                    .process_check(monitor.id, status, check.error_message)
                    .await?
                {
                    events.push(event);
                }
            }
        }

        Ok(events)
    }

    /// Get all active incidents
    pub async fn get_active_incidents(&self) -> Result<Vec<status_incidents::Model>, OutageError> {
        let incidents = status_incidents::Entity::find()
            .filter(status_incidents::Column::Status.ne("resolved"))
            .order_by(status_incidents::Column::StartedAt, Order::Desc)
            .all(self.db.as_ref())
            .await?;

        Ok(incidents)
    }

    /// Get incidents for a specific project
    pub async fn get_project_incidents(
        &self,
        project_id: i32,
        include_resolved: bool,
    ) -> Result<Vec<status_incidents::Model>, OutageError> {
        let mut query = status_incidents::Entity::find()
            .filter(status_incidents::Column::ProjectId.eq(project_id));

        if !include_resolved {
            query = query.filter(status_incidents::Column::Status.ne("resolved"));
        }

        let incidents = query
            .order_by(status_incidents::Column::StartedAt, Order::Desc)
            .all(self.db.as_ref())
            .await?;

        Ok(incidents)
    }

    /// Get current status for all monitors
    pub async fn get_monitor_statuses(&self) -> HashMap<i32, MonitorStatus> {
        let states = self.monitor_states.read().await;
        states
            .iter()
            .map(|(id, state)| (*id, state.status))
            .collect()
    }

    /// Clear cached state for a monitor (e.g., when monitor is deleted)
    pub async fn clear_monitor_state(&self, monitor_id: i32) {
        let mut states = self.monitor_states.write().await;
        states.remove(&monitor_id);
    }

    /// Start event-driven outage detection by listening to StatusCheckCompleted jobs
    /// This is the primary method for detecting outages in real-time.
    pub async fn start_monitoring(self: Arc<Self>, mut job_receiver: Box<dyn JobReceiver>) {
        info!("Starting event-driven outage detection (listening to StatusCheckCompleted jobs)");

        loop {
            match job_receiver.recv().await {
                Ok(Job::StatusCheckCompleted(job)) => {
                    debug!(
                        "Received StatusCheckCompleted job for monitor {} with status {}",
                        job.monitor_id, job.status
                    );

                    let status = MonitorStatus::from_str(&job.status);
                    match self
                        .process_check(job.monitor_id, status, job.error_message)
                        .await
                    {
                        Ok(Some(event)) => {
                            if event.current_status.is_outage() {
                                warn!(
                                    "Monitor {} ({}) is {} - outage detected",
                                    event.monitor_name,
                                    event.monitor_id,
                                    event.current_status.as_str()
                                );
                            } else {
                                info!(
                                    "Monitor {} ({}) recovered - back online",
                                    event.monitor_name, event.monitor_id
                                );
                            }
                        }
                        Ok(None) => {
                            // No state change, no action needed
                            debug!("Monitor {} - no state change", job.monitor_id);
                        }
                        Err(e) => {
                            error!(
                                "Failed to process status check for monitor {}: {:?}",
                                job.monitor_id, e
                            );
                        }
                    }
                }
                Ok(_) => {
                    // Ignore other job types
                }
                Err(e) => {
                    error!("Error receiving job in outage detection service: {:?}", e);
                    tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                }
            }
        }
    }

    /// Scan all monitors - this is now a fallback/diagnostic method
    /// The primary detection method is event-driven via start_monitoring()
    #[allow(dead_code)]
    pub async fn start_monitoring_fallback(self: Arc<Self>, scan_interval_secs: u64) {
        info!(
            "Starting fallback outage detection with polling (interval: {}s)",
            scan_interval_secs
        );

        loop {
            match self.scan_monitors().await {
                Ok(events) => {
                    if !events.is_empty() {
                        info!("Outage scan detected {} state changes", events.len());
                        for event in &events {
                            if event.current_status.is_outage() {
                                warn!(
                                    "Monitor {} ({}) is {}",
                                    event.monitor_name,
                                    event.monitor_id,
                                    event.current_status.as_str()
                                );
                            } else {
                                info!(
                                    "Monitor {} ({}) recovered",
                                    event.monitor_name, event.monitor_id
                                );
                            }
                        }
                    } else {
                        debug!("Outage scan completed, no state changes detected");
                    }
                }
                Err(e) => {
                    error!("Outage scan failed: {}", e);
                }
            }

            tokio::time::sleep(tokio::time::Duration::from_secs(scan_interval_secs)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alarm_service::AlarmService;
    use async_trait::async_trait;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use std::sync::atomic::{AtomicU32, Ordering};
    use temps_core::jobs::QueueError;
    use temps_core::notifications::{EmailMessage, NotificationData, NotificationError};

    struct NoopNotificationService;

    #[async_trait]
    impl NotificationService for NoopNotificationService {
        async fn send_notification(
            &self,
            _notification: NotificationData,
        ) -> Result<(), NotificationError> {
            Ok(())
        }
        async fn send_email(&self, _message: EmailMessage) -> Result<(), NotificationError> {
            Ok(())
        }
        async fn is_configured(&self) -> Result<bool, NotificationError> {
            Ok(false)
        }
    }

    struct NoopJobQueue;

    #[async_trait]
    impl temps_core::JobQueue for NoopJobQueue {
        async fn send(&self, _job: Job) -> Result<(), QueueError> {
            Ok(())
        }
        fn subscribe(&self) -> Box<dyn temps_core::JobReceiver> {
            unimplemented!("not needed in tests")
        }
    }

    fn create_test_alarm_service(db: Arc<DatabaseConnection>) -> Arc<AlarmService> {
        Arc::new(AlarmService::new(
            db,
            Arc::new(NoopNotificationService),
            Arc::new(NoopJobQueue),
        ))
    }

    #[test]
    fn test_monitor_status_parsing() {
        assert_eq!(
            MonitorStatus::from_str("operational"),
            MonitorStatus::Operational
        );
        assert_eq!(MonitorStatus::from_str("degraded"), MonitorStatus::Degraded);
        assert_eq!(
            MonitorStatus::from_str("partial_outage"),
            MonitorStatus::Degraded
        );
        assert_eq!(MonitorStatus::from_str("down"), MonitorStatus::Down);
        assert_eq!(MonitorStatus::from_str("major_outage"), MonitorStatus::Down);
        assert_eq!(
            MonitorStatus::from_str("OPERATIONAL"),
            MonitorStatus::Operational
        );
        assert_eq!(
            MonitorStatus::from_str("unknown"),
            MonitorStatus::Operational
        );
    }

    #[test]
    fn test_monitor_status_is_outage() {
        assert!(!MonitorStatus::Operational.is_outage());
        assert!(MonitorStatus::Degraded.is_outage());
        assert!(MonitorStatus::Down.is_outage());
    }

    #[test]
    fn test_incident_severity_from_status() {
        assert_eq!(
            IncidentSeverity::from_status(MonitorStatus::Degraded),
            IncidentSeverity::Minor
        );
        assert_eq!(
            IncidentSeverity::from_status(MonitorStatus::Down),
            IncidentSeverity::Major
        );
    }

    #[test]
    fn test_incident_severity_priority() {
        assert!(matches!(
            IncidentSeverity::Minor.to_priority(),
            NotificationPriority::Normal
        ));
        assert!(matches!(
            IncidentSeverity::Major.to_priority(),
            NotificationPriority::High
        ));
        assert!(matches!(
            IncidentSeverity::Critical.to_priority(),
            NotificationPriority::Critical
        ));
    }

    #[tokio::test]
    async fn test_clear_monitor_state() {
        let db = Arc::new(sea_orm::Database::connect("sqlite::memory:").await.unwrap());
        let alarm_service = create_test_alarm_service(db.clone());
        let service =
            OutageDetectionService::new(db, Arc::new(NoopNotificationService), alarm_service);

        // Manually insert some monitor states
        {
            let mut states = service.monitor_states.write().await;
            states.insert(
                1,
                MonitorState {
                    status: MonitorStatus::Operational,
                    active_incident_id: None,
                    consecutive_failures: 0,
                },
            );
            states.insert(
                2,
                MonitorState {
                    status: MonitorStatus::Down,
                    active_incident_id: Some(42),
                    consecutive_failures: 5,
                },
            );
            states.insert(
                3,
                MonitorState {
                    status: MonitorStatus::Degraded,
                    active_incident_id: None,
                    consecutive_failures: 1,
                },
            );
        }

        // Verify initial state
        {
            let states = service.monitor_states.read().await;
            assert_eq!(states.len(), 3);
        }

        // Clear monitor 2
        service.clear_monitor_state(2).await;

        {
            let states = service.monitor_states.read().await;
            assert_eq!(states.len(), 2);
            assert!(!states.contains_key(&2), "Monitor 2 should be removed");
            assert!(states.contains_key(&1), "Monitor 1 should remain");
            assert!(states.contains_key(&3), "Monitor 3 should remain");
        }

        // Clear non-existent monitor — should not panic
        service.clear_monitor_state(999).await;

        {
            let states = service.monitor_states.read().await;
            assert_eq!(states.len(), 2, "No change for non-existent monitor");
        }
    }

    #[tokio::test]
    async fn test_get_monitor_statuses() {
        let db = Arc::new(sea_orm::Database::connect("sqlite::memory:").await.unwrap());
        let alarm_service = create_test_alarm_service(db.clone());
        let service =
            OutageDetectionService::new(db, Arc::new(NoopNotificationService), alarm_service);

        // Empty initially
        let statuses = service.get_monitor_statuses().await;
        assert!(statuses.is_empty());

        // Insert states
        {
            let mut states = service.monitor_states.write().await;
            states.insert(
                10,
                MonitorState {
                    status: MonitorStatus::Operational,
                    active_incident_id: None,
                    consecutive_failures: 0,
                },
            );
            states.insert(
                20,
                MonitorState {
                    status: MonitorStatus::Down,
                    active_incident_id: Some(1),
                    consecutive_failures: 3,
                },
            );
        }

        let statuses = service.get_monitor_statuses().await;
        assert_eq!(statuses.len(), 2);
        assert_eq!(statuses[&10], MonitorStatus::Operational);
        assert_eq!(statuses[&20], MonitorStatus::Down);
    }

    #[test]
    fn test_monitor_state_struct() {
        let state = MonitorState {
            status: MonitorStatus::Down,
            active_incident_id: Some(42),
            consecutive_failures: 3,
        };

        assert!(state.status.is_outage());
        assert_eq!(state.active_incident_id, Some(42));
        assert_eq!(state.consecutive_failures, 3);
    }

    #[test]
    fn test_outage_error_display() {
        let db_err = OutageError::Database("connection refused".to_string());
        assert_eq!(db_err.to_string(), "Database error: connection refused");

        let not_found = OutageError::MonitorNotFound(42);
        assert_eq!(not_found.to_string(), "Monitor not found: 42");

        let notif_err = OutageError::Notification("timeout".to_string());
        assert_eq!(notif_err.to_string(), "Notification error: timeout");
    }

    // ── Alarm bridge tests ────────────────────────────────────────────

    /// Notification service that tracks send count
    struct TrackingNotificationService {
        send_count: AtomicU32,
    }

    impl TrackingNotificationService {
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
    impl NotificationService for TrackingNotificationService {
        async fn send_notification(
            &self,
            _notification: NotificationData,
        ) -> Result<(), NotificationError> {
            self.send_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        async fn send_email(&self, _message: EmailMessage) -> Result<(), NotificationError> {
            Ok(())
        }
        async fn is_configured(&self) -> Result<bool, NotificationError> {
            Ok(true)
        }
    }

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

    fn make_environment_model(id: i32, current_deployment_id: Option<i32>) -> environments::Model {
        use temps_entities::upstream_config::UpstreamList;
        environments::Model {
            id,
            name: "production".to_string(),
            slug: "production".to_string(),
            subdomain: "prod".to_string(),
            last_deployment: None,
            host: "example.com".to_string(),
            upstreams: UpstreamList::new(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            project_id: 1,
            current_deployment_id,
            branch: None,
            deleted_at: None,
            deployment_config: None,
            is_preview: false,
            protected: false,
            sleeping: false,
            last_activity_at: None,
        }
    }

    fn make_alarm_model(id: i32, alarm_type: &str, status: &str) -> temps_entities::alarms::Model {
        temps_entities::alarms::Model {
            id,
            project_id: 1,
            environment_id: Some(1),
            deployment_id: Some(10),
            container_id: None,
            service_id: None,
            alarm_type: alarm_type.to_string(),
            severity: "critical".to_string(),
            status: status.to_string(),
            title: "Outage alarm".to_string(),
            message: None,
            metadata: None,
            fired_at: Utc::now(),
            acknowledged_at: None,
            acknowledged_by: None,
            resolved_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn make_outage_event(
        environment_id: Option<i32>,
        current_status: MonitorStatus,
        previous_status: MonitorStatus,
    ) -> OutageEvent {
        OutageEvent {
            monitor_id: 1,
            monitor_name: "API Health".to_string(),
            project_id: 1,
            environment_id,
            previous_status,
            current_status,
            error_message: Some("Connection timeout".to_string()),
            incident_id: Some(42),
            occurred_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn test_fire_outage_alarm_creates_alarm_for_down_status() {
        let alarm_model = make_alarm_model(1, "outage", "firing");

        // Mock DB: environment lookup + cooldown check + alarm insert
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // Environment lookup
            .append_query_results(vec![vec![make_environment_model(1, Some(10))]])
            // Cooldown count query
            .append_query_results([[maplit::btreemap! {
                "num_items" => sea_orm::Value::BigInt(Some(0)),
            }]])
            // Alarm insert result
            .append_query_results(vec![vec![alarm_model]])
            .into_connection();

        let db = Arc::new(db);
        let notification_service = Arc::new(TrackingNotificationService::new());
        let job_queue = Arc::new(TrackingJobQueue::new());

        let alarm_service = Arc::new(AlarmService::new(
            db.clone(),
            notification_service.clone(),
            job_queue.clone(),
        ));

        let service =
            OutageDetectionService::new(db, Arc::new(NoopNotificationService), alarm_service);

        let event = make_outage_event(Some(1), MonitorStatus::Down, MonitorStatus::Operational);
        service.fire_outage_alarm(&event).await;

        // AlarmService should have sent notification + job
        assert_eq!(notification_service.send_count(), 1);
        assert_eq!(job_queue.send_count(), 1);
    }

    #[tokio::test]
    async fn test_fire_outage_alarm_skips_when_no_environment_id() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let db = Arc::new(db);
        let notification_service = Arc::new(TrackingNotificationService::new());
        let job_queue = Arc::new(TrackingJobQueue::new());

        let alarm_service = Arc::new(AlarmService::new(
            db.clone(),
            notification_service.clone(),
            job_queue.clone(),
        ));

        let service =
            OutageDetectionService::new(db, Arc::new(NoopNotificationService), alarm_service);

        // No environment_id => skip alarm
        let event = make_outage_event(None, MonitorStatus::Down, MonitorStatus::Operational);
        service.fire_outage_alarm(&event).await;

        assert_eq!(
            notification_service.send_count(),
            0,
            "No alarm should be fired without environment_id"
        );
        assert_eq!(job_queue.send_count(), 0);
    }

    #[tokio::test]
    async fn test_fire_outage_alarm_skips_when_no_current_deployment() {
        // Environment exists but has no current_deployment_id
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![make_environment_model(1, None)]])
            .into_connection();

        let db = Arc::new(db);
        let notification_service = Arc::new(TrackingNotificationService::new());
        let job_queue = Arc::new(TrackingJobQueue::new());

        let alarm_service = Arc::new(AlarmService::new(
            db.clone(),
            notification_service.clone(),
            job_queue.clone(),
        ));

        let service =
            OutageDetectionService::new(db, Arc::new(NoopNotificationService), alarm_service);

        let event = make_outage_event(Some(1), MonitorStatus::Down, MonitorStatus::Operational);
        service.fire_outage_alarm(&event).await;

        assert_eq!(
            notification_service.send_count(),
            0,
            "No alarm without current deployment"
        );
        assert_eq!(job_queue.send_count(), 0);
    }

    #[tokio::test]
    async fn test_fire_outage_alarm_skips_operational_status() {
        // Even if somehow called with Operational status, should return early
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![make_environment_model(1, Some(10))]])
            .into_connection();

        let db = Arc::new(db);
        let notification_service = Arc::new(TrackingNotificationService::new());
        let job_queue = Arc::new(TrackingJobQueue::new());

        let alarm_service = Arc::new(AlarmService::new(
            db.clone(),
            notification_service.clone(),
            job_queue.clone(),
        ));

        let service =
            OutageDetectionService::new(db, Arc::new(NoopNotificationService), alarm_service);

        let event = make_outage_event(Some(1), MonitorStatus::Operational, MonitorStatus::Down);
        service.fire_outage_alarm(&event).await;

        assert_eq!(
            notification_service.send_count(),
            0,
            "No alarm for operational status"
        );
        assert_eq!(job_queue.send_count(), 0);
    }

    #[tokio::test]
    async fn test_resolve_outage_alarm_resolves_firing_alarms() {
        let alarm = make_alarm_model(1, "outage", "firing");

        // Mock DB: environment lookup + find firing alarms + resolve each (find + update)
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // Environment lookup for resolve_outage_alarm
            .append_query_results(vec![vec![make_environment_model(1, Some(10))]])
            // Find firing outage alarms
            .append_query_results(vec![vec![alarm.clone()]])
            // resolve_alarm: find by id
            .append_query_results(vec![vec![alarm.clone()]])
            // resolve_alarm: update
            .append_query_results(vec![vec![alarm.clone()]])
            .append_exec_results(vec![sea_orm::MockExecResult {
                last_insert_id: 1,
                rows_affected: 1,
            }])
            .into_connection();

        let db = Arc::new(db);
        let notification_service = Arc::new(TrackingNotificationService::new());
        let job_queue = Arc::new(TrackingJobQueue::new());

        let alarm_service = Arc::new(AlarmService::new(
            db.clone(),
            notification_service.clone(),
            job_queue.clone(),
        ));

        let service =
            OutageDetectionService::new(db, Arc::new(NoopNotificationService), alarm_service);

        let event = make_outage_event(Some(1), MonitorStatus::Operational, MonitorStatus::Down);
        service.resolve_outage_alarm(&event).await;

        // Should send resolved notification + job
        assert_eq!(notification_service.send_count(), 1);
        assert_eq!(job_queue.send_count(), 1);
    }

    #[tokio::test]
    async fn test_resolve_outage_alarm_skips_when_no_environment_id() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let db = Arc::new(db);
        let notification_service = Arc::new(TrackingNotificationService::new());
        let job_queue = Arc::new(TrackingJobQueue::new());

        let alarm_service = Arc::new(AlarmService::new(
            db.clone(),
            notification_service.clone(),
            job_queue.clone(),
        ));

        let service =
            OutageDetectionService::new(db, Arc::new(NoopNotificationService), alarm_service);

        let event = make_outage_event(None, MonitorStatus::Operational, MonitorStatus::Down);
        service.resolve_outage_alarm(&event).await;

        assert_eq!(notification_service.send_count(), 0);
        assert_eq!(job_queue.send_count(), 0);
    }

    #[tokio::test]
    async fn test_resolve_outage_alarm_noop_when_no_firing_alarms() {
        // Mock DB: environment lookup + find firing alarms (empty)
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![make_environment_model(1, Some(10))]])
            .append_query_results(vec![Vec::<temps_entities::alarms::Model>::new()])
            .into_connection();

        let db = Arc::new(db);
        let notification_service = Arc::new(TrackingNotificationService::new());
        let job_queue = Arc::new(TrackingJobQueue::new());

        let alarm_service = Arc::new(AlarmService::new(
            db.clone(),
            notification_service.clone(),
            job_queue.clone(),
        ));

        let service =
            OutageDetectionService::new(db, Arc::new(NoopNotificationService), alarm_service);

        let event = make_outage_event(Some(1), MonitorStatus::Operational, MonitorStatus::Down);
        service.resolve_outage_alarm(&event).await;

        assert_eq!(
            notification_service.send_count(),
            0,
            "No notifications when no alarms to resolve"
        );
        assert_eq!(job_queue.send_count(), 0);
    }
}
