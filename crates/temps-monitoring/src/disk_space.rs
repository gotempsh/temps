// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Disk space monitoring service
//!
//! Monitors disk usage and triggers alerts when thresholds are exceeded.

use crate::alarm_service::{AlarmService, AlarmSeverity, AlarmType, FireAlarmRequest};
use anyhow::Result;
use chrono::{DateTime, Utc};
use std::sync::Arc;
use temps_config::ConfigService;
use temps_core::DiskSpaceAlertSettings;
use thiserror::Error;
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

// Disk inspection types and pure collection logic live in `temps-config` so the
// read-only Settings API endpoint can reuse them without depending on this
// (notification-bearing) crate. Re-exported here for back-compat.
pub use temps_config::disk_status::{
    collect_disk_status, format_bytes, get_disk_info, DiskInfo, DiskSpaceAlert,
    DiskSpaceCheckResult,
};

#[derive(Debug, Error)]
pub enum DiskSpaceError {
    #[error("Configuration error: {0}")]
    Configuration(String),
    #[error("Disk not found: {0}")]
    DiskNotFound(String),
    #[error("System error: {0}")]
    System(String),
}

/// Disk space monitoring service
pub struct DiskSpaceMonitor {
    config_service: Arc<ConfigService>,
    alarm_service: Arc<AlarmService>,
    last_alert_time: RwLock<Option<DateTime<Utc>>>,
}

impl DiskSpaceMonitor {
    /// Create a new disk space monitor.
    ///
    /// The monitored disks are resolved from settings via the shared
    /// `temps_config::disk_status` collector: all mounted writable volumes by
    /// default, or only the disk backing `disk_space_alert.monitor_path` when
    /// that is set.
    pub fn new(config_service: Arc<ConfigService>, alarm_service: Arc<AlarmService>) -> Self {
        Self {
            config_service,
            alarm_service,
            last_alert_time: RwLock::new(None),
        }
    }

    /// Get the current disk space settings
    async fn get_settings(&self) -> Result<DiskSpaceAlertSettings, DiskSpaceError> {
        let settings = self
            .config_service
            .get_settings()
            .await
            .map_err(|e| DiskSpaceError::Configuration(e.to_string()))?;
        Ok(settings.disk_space_alert)
    }

    /// Get disk information for all disks or a specific path
    pub fn get_disk_info(&self, path: Option<&str>) -> Result<Vec<DiskInfo>, DiskSpaceError> {
        Ok(get_disk_info(path))
    }

    /// Check disk space against the configured threshold
    pub async fn check_disk_space(&self) -> Result<DiskSpaceCheckResult, DiskSpaceError> {
        collect_disk_status(&self.config_service)
            .await
            .map_err(|e| DiskSpaceError::Configuration(e.to_string()))
    }

    /// Check disk space and send notifications if threshold is exceeded
    pub async fn check_and_notify(&self) -> Result<DiskSpaceCheckResult, DiskSpaceError> {
        let settings = self.get_settings().await?;

        if !settings.enabled {
            debug!("Disk space monitoring is disabled");
            return Ok(DiskSpaceCheckResult {
                checked_at: Utc::now(),
                enabled: false,
                threshold_percent: settings.threshold_percent,
                disks: vec![],
                alerts: vec![],
            });
        }

        let result = self.check_disk_space().await?;

        if !result.alerts.is_empty() {
            self.send_alerts(&result.alerts, &settings).await;
        }

        Ok(result)
    }

    /// Send alert notifications for disks exceeding threshold
    async fn send_alerts(&self, alerts: &[DiskSpaceAlert], settings: &DiskSpaceAlertSettings) {
        for alert in alerts {
            let severity = if alert.usage_percent >= 95.0 {
                AlarmSeverity::Critical
            } else if alert.usage_percent >= 90.0 {
                AlarmSeverity::Warning
            } else {
                AlarmSeverity::Info
            };

            let title = format!(
                "Disk Space Alert: {} at {:.1}%",
                alert.mount_point, alert.usage_percent
            );

            let message = format!(
                "Disk usage on {} has reached {:.1}%, exceeding the configured threshold of {}%.\n\n\
                Available space: {}\n\n\
                Please free up disk space or increase the threshold in Settings > System Monitoring.",
                alert.mount_point,
                alert.usage_percent,
                settings.threshold_percent,
                alert.available_human
            );

            // Host/control-plane-wide: no project/environment/deployment owns
            // "the disk". Note this also means every over-threshold mount
            // shares one cooldown bucket (there's no per-disk scope column to
            // key on) — a second disk breaching seconds after the first can
            // get folded into the same cooldown window rather than alerting
            // independently. Acceptable for the common single/few-disk case;
            // revisit if multi-disk instances report missed alerts.
            let request = FireAlarmRequest {
                project_id: None,
                environment_id: None,
                deployment_id: None,
                container_id: None,
                service_id: None,
                alarm_type: AlarmType::DiskSpaceLow,
                severity,
                title,
                message,
                metadata: Some(serde_json::json!({
                    "mount_point": alert.mount_point,
                    "usage_percent": format!("{:.1}", alert.usage_percent),
                    "threshold_percent": settings.threshold_percent,
                    "available_bytes": alert.available_human,
                })),
            };

            match self.alarm_service.fire_alarm(request).await {
                Ok(Some(_)) => {
                    info!(
                        "Sent disk space alert for {} ({:.1}%)",
                        alert.mount_point, alert.usage_percent
                    );

                    // Update last alert time
                    let mut last_alert = self.last_alert_time.write().await;
                    *last_alert = Some(Utc::now());
                }
                Ok(None) => {
                    debug!(
                        "Disk space alert for {} suppressed by cooldown/silence",
                        alert.mount_point
                    );
                }
                Err(e) => {
                    error!(
                        "Failed to fire disk space alarm for {}: {}",
                        alert.mount_point, e
                    );
                }
            }
        }
    }

    /// Start the background monitoring task
    pub async fn start_monitoring(self: Arc<Self>) {
        info!("Starting disk space monitoring");

        loop {
            let settings = match self.get_settings().await {
                Ok(s) => s,
                Err(e) => {
                    error!("Failed to get disk space settings: {}", e);
                    tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
                    continue;
                }
            };

            if !settings.enabled {
                debug!("Disk space monitoring is disabled, sleeping for 60 seconds");
                tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
                continue;
            }

            match self.check_and_notify().await {
                Ok(result) => {
                    if result.alerts.is_empty() {
                        debug!(
                            "Disk space check completed: {} disk(s) monitored, all within threshold",
                            result.disks.len()
                        );
                    } else {
                        warn!(
                            "Disk space check completed: {} disk(s) exceeding threshold",
                            result.alerts.len()
                        );
                    }
                }
                Err(e) => {
                    error!("Disk space check failed: {}", e);
                }
            }

            tokio::time::sleep(tokio::time::Duration::from_secs(
                settings.check_interval_seconds,
            ))
            .await;
        }
    }

    /// Get the last time an alert was sent
    pub async fn last_alert_time(&self) -> Option<DateTime<Utc>> {
        *self.last_alert_time.read().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // NOTE: disk inspection + threshold/format logic is owned and unit-tested in
    // `temps_config::disk_status`. The tests here cover only the
    // notification-bearing behaviour that lives in this crate.

    #[test]
    fn test_disk_space_alert_severity_levels() {
        // Severity is derived from usage_percent in `send_alerts`.
        let create_alert = |usage: f64| DiskSpaceAlert {
            mount_point: "/".to_string(),
            usage_percent: usage,
            threshold_percent: 80,
            available_bytes: 1024 * 1024 * 1024,
            available_human: "1.00 GB".to_string(),
        };

        let normal_alert = create_alert(85.0);
        assert!(normal_alert.usage_percent < 90.0);

        let high_alert = create_alert(92.0);
        assert!(high_alert.usage_percent >= 90.0 && high_alert.usage_percent < 95.0);

        let critical_alert = create_alert(97.0);
        assert!(critical_alert.usage_percent >= 95.0);
    }

    // Minimal mocks for exercising `send_alerts` -> `AlarmService::fire_alarm`.
    struct MockNotificationService {
        notifications_sent: AtomicUsize,
    }

    impl MockNotificationService {
        fn new() -> Self {
            Self {
                notifications_sent: AtomicUsize::new(0),
            }
        }

        fn notification_count(&self) -> usize {
            self.notifications_sent.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl temps_core::notifications::NotificationService for MockNotificationService {
        async fn send_email(
            &self,
            _message: temps_core::notifications::EmailMessage,
        ) -> std::result::Result<(), temps_core::notifications::NotificationError> {
            Ok(())
        }

        async fn send_notification(
            &self,
            _notification: temps_core::notifications::NotificationData,
        ) -> std::result::Result<(), temps_core::notifications::NotificationError> {
            self.notifications_sent.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn is_configured(
            &self,
        ) -> std::result::Result<bool, temps_core::notifications::NotificationError> {
            Ok(true)
        }
    }

    struct NoopJobQueue;

    #[async_trait]
    impl temps_core::JobQueue for NoopJobQueue {
        async fn send(&self, _job: temps_core::Job) -> Result<(), temps_core::jobs::QueueError> {
            Ok(())
        }
        fn subscribe(&self) -> Box<dyn temps_core::JobReceiver> {
            unimplemented!("not needed in tests")
        }
    }

    /// `send_alerts` must persist a `disk_space_low` alarm (not just send a
    /// bare notification) so it shows up in `/monitoring/alarms` and can be
    /// acknowledged/silenced — this is the exact gap that motivated routing
    /// disk-space alerts through `AlarmService::fire_alarm`.
    #[tokio::test]
    async fn test_send_alerts_fires_alarm_via_alarm_service() {
        use sea_orm::{DatabaseBackend, MockDatabase};

        let inserted_alarm = temps_entities::alarms::Model {
            id: 1,
            project_id: None,
            environment_id: None,
            deployment_id: None,
            container_id: None,
            service_id: None,
            alarm_type: AlarmType::DiskSpaceLow.as_str().to_string(),
            severity: AlarmSeverity::Warning.as_str().to_string(),
            status: "firing".to_string(),
            title: "Disk Space Alert: / at 92.0%".to_string(),
            message: None,
            metadata: None,
            fired_at: Utc::now(),
            acknowledged_at: None,
            acknowledged_by: None,
            resolved_at: None,
            silenced_until: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // is_in_cooldown: no recent/silenced alarm for this scope.
            .append_query_results([[maplit::btreemap! {
                "num_items" => sea_orm::Value::BigInt(Some(0)),
            }]])
            // insert (Postgres RETURNING * comes back as a query result)
            .append_query_results(vec![vec![inserted_alarm]])
            .into_connection();

        let notification_service = Arc::new(MockNotificationService::new());
        let alarm_service = Arc::new(AlarmService::new(
            Arc::new(db),
            notification_service.clone(),
            Arc::new(NoopJobQueue),
        ));

        let config_service = {
            let server_config = Arc::new(
                temps_config::ServerConfig::new(
                    "127.0.0.1:3000".to_string(),
                    "postgresql://test".to_string(),
                    None,
                    Some("127.0.0.1:8000".to_string()),
                )
                .unwrap(),
            );
            Arc::new(ConfigService::new(
                server_config,
                Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
            ))
        };
        let monitor = DiskSpaceMonitor::new(config_service, alarm_service.clone());

        let alert = DiskSpaceAlert {
            mount_point: "/".to_string(),
            usage_percent: 92.0,
            threshold_percent: 80,
            available_bytes: 1024 * 1024 * 1024,
            available_human: "1.00 GB".to_string(),
        };
        let settings = DiskSpaceAlertSettings {
            threshold_percent: 80,
            ..Default::default()
        };

        monitor.send_alerts(&[alert], &settings).await;

        assert_eq!(
            notification_service.notification_count(),
            1,
            "fire_alarm should have persisted the alarm AND sent one notification"
        );
    }

    #[test]
    fn test_disk_space_error_display() {
        let config_err = DiskSpaceError::Configuration("test config error".to_string());
        assert!(config_err.to_string().contains("Configuration error"));

        let disk_err = DiskSpaceError::DiskNotFound("/nonexistent".to_string());
        assert!(disk_err.to_string().contains("Disk not found"));

        let sys_err = DiskSpaceError::System("system failure".to_string());
        assert!(sys_err.to_string().contains("System error"));
    }
}
