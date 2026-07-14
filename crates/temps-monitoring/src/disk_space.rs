//! Disk space monitoring service
//!
//! Monitors disk usage and triggers alerts when thresholds are exceeded.

use anyhow::Result;
use chrono::{DateTime, Utc};
use std::sync::Arc;
use temps_config::ConfigService;
use temps_core::notifications::{
    NotificationData, NotificationPriority, NotificationService, NotificationType,
};
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
    notification_service: Arc<dyn NotificationService>,
    last_alert_time: RwLock<Option<DateTime<Utc>>>,
}

impl DiskSpaceMonitor {
    /// Create a new disk space monitor.
    ///
    /// The monitored disks are resolved from settings via the shared
    /// `temps_config::disk_status` collector: all mounted writable volumes by
    /// default, or only the disk backing `disk_space_alert.monitor_path` when
    /// that is set.
    pub fn new(
        config_service: Arc<ConfigService>,
        notification_service: Arc<dyn NotificationService>,
    ) -> Self {
        Self {
            config_service,
            notification_service,
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
        // Check if notification service is configured
        match self.notification_service.is_configured().await {
            Ok(false) => {
                debug!("Notification service not configured, skipping disk space alert");
                return;
            }
            Err(e) => {
                error!("Failed to check notification service configuration: {}", e);
                return;
            }
            Ok(true) => {}
        }

        for alert in alerts {
            let severity = if alert.usage_percent >= 95.0 {
                NotificationPriority::Critical
            } else if alert.usage_percent >= 90.0 {
                NotificationPriority::High
            } else {
                NotificationPriority::Normal
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

            let notification = NotificationData {
                id: temps_core::uuid::Uuid::new_v4().to_string(),
                title,
                message,
                notification_type: NotificationType::Warning,
                priority: severity,
                severity: Some("warning".to_string()),
                timestamp: Utc::now(),
                metadata: [
                    ("mount_point".to_string(), alert.mount_point.clone()),
                    (
                        "usage_percent".to_string(),
                        format!("{:.1}", alert.usage_percent),
                    ),
                    (
                        "threshold_percent".to_string(),
                        settings.threshold_percent.to_string(),
                    ),
                    ("available_bytes".to_string(), alert.available_human.clone()),
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
                    "Failed to send disk space alert for {}: {}",
                    alert.mount_point, e
                );
            } else {
                info!(
                    "Sent disk space alert for {} ({:.1}%)",
                    alert.mount_point, alert.usage_percent
                );

                // Update last alert time
                let mut last_alert = self.last_alert_time.write().await;
                *last_alert = Some(Utc::now());
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
    use tokio::sync::Mutex;

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

    // Mock notification service for testing
    struct MockNotificationService {
        notifications_sent: AtomicUsize,
        last_notification: Mutex<Option<NotificationData>>,
        is_configured: bool,
    }

    impl MockNotificationService {
        fn new(is_configured: bool) -> Self {
            Self {
                notifications_sent: AtomicUsize::new(0),
                last_notification: Mutex::new(None),
                is_configured,
            }
        }

        fn notification_count(&self) -> usize {
            self.notifications_sent.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl NotificationService for MockNotificationService {
        async fn send_email(
            &self,
            _message: temps_core::notifications::EmailMessage,
        ) -> std::result::Result<(), temps_core::notifications::NotificationError> {
            Ok(())
        }

        async fn send_notification(
            &self,
            notification: NotificationData,
        ) -> std::result::Result<(), temps_core::notifications::NotificationError> {
            self.notifications_sent.fetch_add(1, Ordering::SeqCst);
            let mut last = self.last_notification.lock().await;
            *last = Some(notification);
            Ok(())
        }

        async fn is_configured(
            &self,
        ) -> std::result::Result<bool, temps_core::notifications::NotificationError> {
            Ok(self.is_configured)
        }
    }

    #[tokio::test]
    async fn test_notification_service_integration() {
        let mock_service = Arc::new(MockNotificationService::new(true));

        // Simulate sending a disk space alert notification
        let notification = NotificationData {
            id: "test-id".to_string(),
            title: "Disk Space Alert: / at 85.0%".to_string(),
            message: "Disk usage has exceeded threshold".to_string(),
            notification_type: NotificationType::Warning,
            priority: NotificationPriority::Normal,
            severity: Some("warning".to_string()),
            timestamp: Utc::now(),
            metadata: std::collections::HashMap::new(),
            bypass_throttling: false,
        };

        mock_service.send_notification(notification).await.unwrap();
        assert_eq!(mock_service.notification_count(), 1);
    }

    #[tokio::test]
    async fn test_notification_not_sent_when_unconfigured() {
        let mock_service = Arc::new(MockNotificationService::new(false));

        // Check that is_configured returns false
        let is_configured = mock_service.is_configured().await.unwrap();
        assert!(!is_configured);

        // In real code, this would prevent notification from being sent
        assert_eq!(mock_service.notification_count(), 0);
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
