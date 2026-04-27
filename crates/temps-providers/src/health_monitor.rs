//! External service health monitor.
//!
//! Periodically probes every `external_services` row where `status = 'running'`
//! via a TCP connect to the service's effective address. Records each probe
//! in `external_service_health_checks` and updates the denormalized
//! `health_status` / `last_health_check_at` / `last_health_error` columns
//! on `external_services` so the UI can render a status badge in one query.
//!
//! When a service fails `CONSECUTIVE_FAILURES_BEFORE_ALERT` probes in a row,
//! the monitor sends a notification via the shared `NotificationService`.
//! A recovery notification is sent when the service returns to `operational`.

use crate::externalsvc::{HealthProbeStatus, ServiceType};
use crate::services::ExternalServiceManager;
use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter,
};
use std::sync::Arc;
use std::time::Duration;
use temps_core::notifications::{
    NotificationData, NotificationPriority, NotificationService, NotificationType,
};
use temps_entities::{external_service_health_checks, external_services};
use thiserror::Error;
use tracing::{debug, error, info, warn};

/// How many failed probes in a row before we raise an alert.
const CONSECUTIVE_FAILURES_BEFORE_ALERT: i32 = 3;

/// Configuration for `ExternalServiceHealthMonitor`.
#[derive(Debug, Clone)]
pub struct ExternalServiceHealthConfig {
    /// How often to run a full check cycle (seconds).
    pub poll_interval_secs: u64,
    /// How many days of check history to keep before pruning. 0 disables pruning.
    pub retention_days: i64,
}

impl Default for ExternalServiceHealthConfig {
    fn default() -> Self {
        Self {
            poll_interval_secs: 30,
            retention_days: 30,
        }
    }
}

// Status strings come from `HealthProbeStatus::as_str` (operational|degraded|down)
// so the `external_service_health_checks.status` column stays in sync with
// the trait-level result type.

#[derive(Debug, Error)]
pub enum HealthMonitorError {
    #[error("Database error: {0}")]
    Database(#[from] sea_orm::DbErr),

    #[error("External service {id} not found")]
    ServiceNotFound { id: i32 },
}

/// Background loop that keeps `external_services.health_status` in sync with
/// reality and sends alerts when a service stays down for 3+ consecutive checks.
pub struct ExternalServiceHealthMonitor {
    db: Arc<DatabaseConnection>,
    manager: Arc<ExternalServiceManager>,
    notification_service: Arc<dyn NotificationService>,
    config: ExternalServiceHealthConfig,
}

impl ExternalServiceHealthMonitor {
    pub fn new(
        db: Arc<DatabaseConnection>,
        manager: Arc<ExternalServiceManager>,
        notification_service: Arc<dyn NotificationService>,
        config: ExternalServiceHealthConfig,
    ) -> Self {
        Self {
            db,
            manager,
            notification_service,
            config,
        }
    }

    /// Run forever. Spawn this onto a background task.
    pub async fn start(self: Arc<Self>) {
        info!(
            "Starting external service health monitor (poll interval: {}s)",
            self.config.poll_interval_secs
        );

        let mut prune_counter: u32 = 0;

        loop {
            if let Err(e) = self.run_cycle().await {
                error!("External service health check cycle failed: {}", e);
            }

            // Once an hour, prune old check rows.
            prune_counter = prune_counter.wrapping_add(1);
            if self.config.retention_days > 0
                && prune_counter
                    .is_multiple_of((3600 / self.config.poll_interval_secs.max(1)).max(1) as u32)
            {
                if let Err(e) = self.prune_old_checks().await {
                    warn!("Health check pruning failed: {}", e);
                }
            }

            tokio::time::sleep(Duration::from_secs(self.config.poll_interval_secs)).await;
        }
    }

    async fn run_cycle(&self) -> Result<(), HealthMonitorError> {
        let services = external_services::Entity::find()
            .all(self.db.as_ref())
            .await?;

        if services.is_empty() {
            debug!("No external services to health-check");
            return Ok(());
        }

        debug!("Health-checking {} external service(s)", services.len());

        for service in services {
            if let Err(e) = self.check_service(&service).await {
                warn!(
                    "Health check error for service {} ({}): {}",
                    service.id, service.name, e
                );
            }
        }

        Ok(())
    }

    /// Run a single health check for one service on demand (e.g. triggered by
    /// a user via the REST API). Writes the same history row + denormalized
    /// fields as the background loop and fires alerts on the Nth consecutive
    /// failure / recovery, so the consecutive-failure counter stays honest.
    pub async fn run_check_for(&self, service_id: i32) -> Result<(), HealthMonitorError> {
        let service = external_services::Entity::find_by_id(service_id)
            .one(self.db.as_ref())
            .await?
            .ok_or(HealthMonitorError::ServiceNotFound { id: service_id })?;

        self.check_service(&service).await
    }

    /// Check one service and record the result.
    async fn check_service(
        &self,
        service: &external_services::Model,
    ) -> Result<(), HealthMonitorError> {
        // Services that aren't supposed to be running should not be probed —
        // we just record them as down without false alerting (alert is gated
        // on consecutive failures and a stopped service starts at 0).
        let (status, response_time_ms, error_message) = if service.status != "running" {
            (
                HealthProbeStatus::Down,
                None,
                Some(format!(
                    "Service status is '{}', not running",
                    service.status
                )),
            )
        } else {
            self.probe_service(service).await
        };

        let now = Utc::now();

        // 1. Append history row
        let history = external_service_health_checks::ActiveModel {
            service_id: Set(service.id),
            checked_at: Set(now),
            status: Set(status.as_str().to_string()),
            response_time_ms: Set(response_time_ms),
            error_message: Set(error_message.clone()),
            ..Default::default()
        };
        if let Err(e) = history.insert(self.db.as_ref()).await {
            warn!(
                "Failed to record health check for service {}: {}",
                service.id, e
            );
        }

        // 2. Update denormalized fields on external_services
        let was_failing = service.consecutive_health_failures;
        let now_failing = if matches!(status, HealthProbeStatus::Down) {
            was_failing + 1
        } else {
            0
        };

        let mut active: external_services::ActiveModel = service.clone().into();
        active.health_status = Set(Some(status.as_str().to_string()));
        active.last_health_check_at = Set(Some(now));
        active.last_health_error = Set(error_message.clone());
        active.consecutive_health_failures = Set(now_failing);
        if let Err(e) = active.update(self.db.as_ref()).await {
            warn!(
                "Failed to update health_status on service {}: {}",
                service.id, e
            );
        }

        // 3. Fire alerts on state transitions
        //    - Down for the Nth consecutive time → alert
        //    - Just recovered from N+ failures → recovery notice
        if matches!(status, HealthProbeStatus::Down)
            && now_failing == CONSECUTIVE_FAILURES_BEFORE_ALERT
        {
            self.send_down_alert(service, error_message.as_deref())
                .await;
        } else if !matches!(status, HealthProbeStatus::Down)
            && was_failing >= CONSECUTIVE_FAILURES_BEFORE_ALERT
        {
            self.send_recovered_alert(service).await;
        }

        Ok(())
    }

    /// Probe the service using its engine-specific health_probe implementation
    /// (Postgres `SELECT 1`, Redis `PING`, MongoDB `ping`, S3/RustFS `ListBuckets`).
    /// Returns (status, response_time_ms, error_message).
    async fn probe_service(
        &self,
        service: &external_services::Model,
    ) -> (HealthProbeStatus, Option<i32>, Option<String>) {
        let service_type = match ServiceType::from_str(&service.service_type) {
            Ok(t) => t,
            Err(_) => {
                return (
                    HealthProbeStatus::Down,
                    None,
                    Some(format!("Unknown service type: {}", service.service_type)),
                );
            }
        };

        // Cluster services need a fan-out probe — the standalone
        // ExternalService::health_probe path can't reach a multi-host
        // cluster (it falls through to localhost:5432). Route through the
        // manager's cluster-aware probe instead.
        if service.topology == "cluster" {
            let result = self.manager.probe_cluster(service).await;
            return (result.status, result.response_time_ms, result.error_message);
        }

        let service_config = match self.manager.get_service_config(service.id).await {
            Ok(cfg) => cfg,
            Err(e) => {
                return (
                    HealthProbeStatus::Down,
                    None,
                    Some(format!("Failed to load service config: {}", e)),
                );
            }
        };

        let instance = self
            .manager
            .get_service_instance(service.name.clone(), service_type);

        match instance.health_probe(service_config).await {
            Ok(result) => (result.status, result.response_time_ms, result.error_message),
            Err(e) => (
                HealthProbeStatus::Down,
                None,
                Some(format!("health_probe raised an error: {}", e)),
            ),
        }
    }

    async fn send_down_alert(
        &self,
        service: &external_services::Model,
        error_message: Option<&str>,
    ) {
        let title = format!("Service down: {}", service.name);
        let message = format!(
            "External service '{}' ({}) has failed {} consecutive health checks.\n\n\
             Last error: {}",
            service.name,
            service.service_type,
            CONSECUTIVE_FAILURES_BEFORE_ALERT,
            error_message.unwrap_or("(no details)")
        );

        let notification = NotificationData {
            id: uuid::Uuid::new_v4().to_string(),
            title,
            message,
            notification_type: NotificationType::Error,
            priority: NotificationPriority::Critical,
            severity: Some("critical".to_string()),
            timestamp: Utc::now(),
            metadata: [
                ("source".to_string(), "external_service_health".to_string()),
                ("service_id".to_string(), service.id.to_string()),
                ("service_name".to_string(), service.name.clone()),
                ("service_type".to_string(), service.service_type.clone()),
            ]
            .into_iter()
            .collect(),
            bypass_throttling: true,
        };

        if let Err(e) = self
            .notification_service
            .send_notification(notification)
            .await
        {
            error!(
                "Failed to send down-alert notification for service {}: {}",
                service.id, e
            );
        } else {
            info!(
                "Sent health-check down alert for service {} ({})",
                service.id, service.name
            );
        }
    }

    async fn send_recovered_alert(&self, service: &external_services::Model) {
        let title = format!("Service recovered: {}", service.name);
        let message = format!(
            "External service '{}' ({}) is responding to health checks again.",
            service.name, service.service_type,
        );

        let notification = NotificationData {
            id: uuid::Uuid::new_v4().to_string(),
            title,
            message,
            notification_type: NotificationType::Info,
            priority: NotificationPriority::Normal,
            severity: None,
            timestamp: Utc::now(),
            metadata: [
                ("source".to_string(), "external_service_health".to_string()),
                ("service_id".to_string(), service.id.to_string()),
                ("service_name".to_string(), service.name.clone()),
                ("status".to_string(), "recovered".to_string()),
            ]
            .into_iter()
            .collect(),
            bypass_throttling: false,
        };

        if let Err(e) = self
            .notification_service
            .send_notification(notification)
            .await
        {
            error!(
                "Failed to send recovery notification for service {}: {}",
                service.id, e
            );
        }
    }

    async fn prune_old_checks(&self) -> Result<(), HealthMonitorError> {
        let cutoff = Utc::now() - chrono::Duration::days(self.config.retention_days);
        let deleted = external_service_health_checks::Entity::delete_many()
            .filter(external_service_health_checks::Column::CheckedAt.lt(cutoff))
            .exec(self.db.as_ref())
            .await?;
        if deleted.rows_affected > 0 {
            info!(
                "Pruned {} external_service_health_checks rows older than {} days",
                deleted.rows_affected, self.config.retention_days
            );
        }
        Ok(())
    }
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_status_strings() {
        assert_eq!(HealthProbeStatus::Operational.as_str(), "operational");
        assert_eq!(HealthProbeStatus::Degraded.as_str(), "degraded");
        assert_eq!(HealthProbeStatus::Down.as_str(), "down");
    }
}
