//! Outage detection and notification service
//!
//! Monitors status checks for state transitions and sends alerts when outages occur.

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
use temps_core::{Job, JobReceiver};
use temps_entities::{status_checks, status_incidents, status_monitors};
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
            "degraded" => Self::Degraded,
            "down" => Self::Down,
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
    ) -> Self {
        Self {
            db,
            notification_service,
            monitor_states: RwLock::new(HashMap::new()),
            failure_threshold: 2, // Alert after 2 consecutive failures
            alert_cooldown: Duration::minutes(5),
        }
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
                states.insert(
                    monitor_id,
                    MonitorState {
                        status,
                        active_incident_id: None,
                        consecutive_failures: if status.is_outage() { 1 } else { 0 },
                    },
                );

                // Don't alert on first check even if it's down
                (false, None)
            }
        };

        drop(states);

        // Handle incident creation/resolution and notifications
        if let Some(ref event) = event {
            self.handle_outage_event(event).await?;
        }

        Ok(event)
    }

    /// Handle an outage event: create/resolve incidents and send notifications
    async fn handle_outage_event(&self, event: &OutageEvent) -> Result<(), OutageError> {
        if event.current_status.is_outage() {
            // New outage - create incident
            let incident_id = self.create_incident(event).await?;
            self.send_outage_notification(event, incident_id).await?;

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
        }

        Ok(())
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

    #[test]
    fn test_monitor_status_parsing() {
        assert_eq!(
            MonitorStatus::from_str("operational"),
            MonitorStatus::Operational
        );
        assert_eq!(MonitorStatus::from_str("degraded"), MonitorStatus::Degraded);
        assert_eq!(MonitorStatus::from_str("down"), MonitorStatus::Down);
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
        use async_trait::async_trait;
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

        let db = Arc::new(sea_orm::Database::connect("sqlite::memory:").await.unwrap());
        let service = OutageDetectionService::new(db, Arc::new(NoopNotificationService));

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
        use async_trait::async_trait;
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

        let db = Arc::new(sea_orm::Database::connect("sqlite::memory:").await.unwrap());
        let service = OutageDetectionService::new(db, Arc::new(NoopNotificationService));

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
}
