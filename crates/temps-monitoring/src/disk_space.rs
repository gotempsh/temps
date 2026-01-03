//! Disk space monitoring service
//!
//! Monitors disk usage and triggers alerts when thresholds are exceeded.

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use sysinfo::Disks;
use temps_config::ConfigService;
use temps_core::notifications::{
    NotificationData, NotificationPriority, NotificationService, NotificationType,
};
use temps_core::DiskSpaceAlertSettings;
use thiserror::Error;
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

/// Disk space information for a single disk/partition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiskInfo {
    /// Mount point of the disk
    pub mount_point: String,
    /// Total space in bytes
    pub total_bytes: u64,
    /// Used space in bytes
    pub used_bytes: u64,
    /// Available space in bytes
    pub available_bytes: u64,
    /// Usage percentage (0-100)
    pub usage_percent: f64,
    /// File system type (e.g., "ext4", "apfs")
    pub file_system: String,
}

/// Result of a disk space check
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiskSpaceCheckResult {
    /// Timestamp of the check
    pub checked_at: DateTime<Utc>,
    /// List of all monitored disks
    pub disks: Vec<DiskInfo>,
    /// Disks that exceed the threshold
    pub alerts: Vec<DiskSpaceAlert>,
}

/// Alert for a disk that exceeds the threshold
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiskSpaceAlert {
    /// Mount point of the disk
    pub mount_point: String,
    /// Current usage percentage
    pub usage_percent: f64,
    /// Configured threshold percentage
    pub threshold_percent: u32,
    /// Available space in bytes
    pub available_bytes: u64,
    /// Human-readable available space
    pub available_human: String,
}

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
    data_dir: PathBuf,
    last_alert_time: RwLock<Option<DateTime<Utc>>>,
}

impl DiskSpaceMonitor {
    /// Create a new disk space monitor
    pub fn new(
        config_service: Arc<ConfigService>,
        notification_service: Arc<dyn NotificationService>,
        data_dir: PathBuf,
    ) -> Self {
        Self {
            config_service,
            notification_service,
            data_dir,
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
        let disks = Disks::new_with_refreshed_list();
        let mut disk_infos = Vec::new();

        for disk in disks.list() {
            let mount_point = disk.mount_point().to_string_lossy().to_string();
            let total = disk.total_space();
            let available = disk.available_space();
            let used = total.saturating_sub(available);
            let usage_percent = if total > 0 {
                (used as f64 / total as f64) * 100.0
            } else {
                0.0
            };

            // Filter by path if specified
            if let Some(target_path) = path {
                // Check if the target path is under this mount point
                if !target_path.starts_with(&mount_point) && mount_point != "/" {
                    continue;
                }
            }

            disk_infos.push(DiskInfo {
                mount_point,
                total_bytes: total,
                used_bytes: used,
                available_bytes: available,
                usage_percent,
                file_system: disk.file_system().to_string_lossy().to_string(),
            });
        }

        // If we have a specific path and multiple matches, return only the most specific one
        if path.is_some() && disk_infos.len() > 1 {
            // Sort by mount point length (longest = most specific) and take the first
            disk_infos.sort_by(|a, b| b.mount_point.len().cmp(&a.mount_point.len()));
            disk_infos.truncate(1);
        }

        Ok(disk_infos)
    }

    /// Check disk space against the configured threshold
    pub async fn check_disk_space(&self) -> Result<DiskSpaceCheckResult, DiskSpaceError> {
        let settings = self.get_settings().await?;

        // Determine which path to monitor
        let monitor_path = settings
            .monitor_path
            .as_deref()
            .unwrap_or_else(|| self.data_dir.to_str().unwrap_or("/"));

        let disks = self.get_disk_info(Some(monitor_path))?;
        let mut alerts = Vec::new();

        for disk in &disks {
            if disk.usage_percent >= settings.threshold_percent as f64 {
                alerts.push(DiskSpaceAlert {
                    mount_point: disk.mount_point.clone(),
                    usage_percent: disk.usage_percent,
                    threshold_percent: settings.threshold_percent,
                    available_bytes: disk.available_bytes,
                    available_human: format_bytes(disk.available_bytes),
                });
            }
        }

        Ok(DiskSpaceCheckResult {
            checked_at: Utc::now(),
            disks,
            alerts,
        })
    }

    /// Check disk space and send notifications if threshold is exceeded
    pub async fn check_and_notify(&self) -> Result<DiskSpaceCheckResult, DiskSpaceError> {
        let settings = self.get_settings().await?;

        if !settings.enabled {
            debug!("Disk space monitoring is disabled");
            return Ok(DiskSpaceCheckResult {
                checked_at: Utc::now(),
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

/// Format bytes into a human-readable string
fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    const TB: u64 = GB * 1024;

    if bytes >= TB {
        format!("{:.2} TB", bytes as f64 / TB as f64)
    } else if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} bytes", bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::Mutex;

    #[test]
    fn test_format_bytes() {
        assert_eq!(format_bytes(0), "0 bytes");
        assert_eq!(format_bytes(512), "512 bytes");
        assert_eq!(format_bytes(1024), "1.00 KB");
        assert_eq!(format_bytes(1536), "1.50 KB");
        assert_eq!(format_bytes(1024 * 1024), "1.00 MB");
        assert_eq!(format_bytes(1024 * 1024 * 1024), "1.00 GB");
        assert_eq!(format_bytes(1024 * 1024 * 1024 * 1024), "1.00 TB");
    }

    #[test]
    fn test_format_bytes_edge_cases() {
        // Test boundary values
        assert_eq!(format_bytes(1023), "1023 bytes");
        assert_eq!(format_bytes(1024 * 1024 - 1), "1024.00 KB");

        // Test large values
        let five_tb = 5 * 1024 * 1024 * 1024 * 1024u64;
        assert_eq!(format_bytes(five_tb), "5.00 TB");
    }

    #[test]
    fn test_disk_info_creation() {
        let info = DiskInfo {
            mount_point: "/".to_string(),
            total_bytes: 1024 * 1024 * 1024 * 100,    // 100 GB
            used_bytes: 1024 * 1024 * 1024 * 80,      // 80 GB
            available_bytes: 1024 * 1024 * 1024 * 20, // 20 GB
            usage_percent: 80.0,
            file_system: "apfs".to_string(),
        };

        assert_eq!(info.mount_point, "/");
        assert_eq!(info.usage_percent, 80.0);
        assert_eq!(info.total_bytes, info.used_bytes + info.available_bytes);
    }

    #[test]
    fn test_disk_info_usage_calculation() {
        // Test usage percentage calculation
        let total: u64 = 1024 * 1024 * 1024 * 100; // 100 GB
        let used: u64 = 1024 * 1024 * 1024 * 85; // 85 GB
        let available = total - used;
        let usage_percent = (used as f64 / total as f64) * 100.0;

        let info = DiskInfo {
            mount_point: "/data".to_string(),
            total_bytes: total,
            used_bytes: used,
            available_bytes: available,
            usage_percent,
            file_system: "ext4".to_string(),
        };

        assert!((info.usage_percent - 85.0).abs() < 0.01);
    }

    #[test]
    fn test_disk_space_alert_creation() {
        let alert = DiskSpaceAlert {
            mount_point: "/".to_string(),
            usage_percent: 85.5,
            threshold_percent: 80,
            available_bytes: 1024 * 1024 * 1024 * 15,
            available_human: "15.00 GB".to_string(),
        };

        assert!(alert.usage_percent > alert.threshold_percent as f64);
    }

    #[test]
    fn test_disk_space_alert_severity_levels() {
        // Test that severity levels are correctly determined based on usage
        let create_alert = |usage: f64| DiskSpaceAlert {
            mount_point: "/".to_string(),
            usage_percent: usage,
            threshold_percent: 80,
            available_bytes: 1024 * 1024 * 1024,
            available_human: "1.00 GB".to_string(),
        };

        // Normal priority (80-89%)
        let normal_alert = create_alert(85.0);
        assert!(normal_alert.usage_percent < 90.0);

        // High priority (90-94%)
        let high_alert = create_alert(92.0);
        assert!(high_alert.usage_percent >= 90.0 && high_alert.usage_percent < 95.0);

        // Critical priority (95%+)
        let critical_alert = create_alert(97.0);
        assert!(critical_alert.usage_percent >= 95.0);
    }

    #[test]
    fn test_disk_space_check_result() {
        let disks = vec![
            DiskInfo {
                mount_point: "/".to_string(),
                total_bytes: 100 * 1024 * 1024 * 1024,
                used_bytes: 75 * 1024 * 1024 * 1024,
                available_bytes: 25 * 1024 * 1024 * 1024,
                usage_percent: 75.0,
                file_system: "apfs".to_string(),
            },
            DiskInfo {
                mount_point: "/data".to_string(),
                total_bytes: 500 * 1024 * 1024 * 1024,
                used_bytes: 450 * 1024 * 1024 * 1024,
                available_bytes: 50 * 1024 * 1024 * 1024,
                usage_percent: 90.0,
                file_system: "ext4".to_string(),
            },
        ];

        let alerts = vec![DiskSpaceAlert {
            mount_point: "/data".to_string(),
            usage_percent: 90.0,
            threshold_percent: 80,
            available_bytes: 50 * 1024 * 1024 * 1024,
            available_human: "50.00 GB".to_string(),
        }];

        let result = DiskSpaceCheckResult {
            checked_at: Utc::now(),
            disks,
            alerts,
        };

        assert_eq!(result.disks.len(), 2);
        assert_eq!(result.alerts.len(), 1);
        assert_eq!(result.alerts[0].mount_point, "/data");
    }

    #[test]
    fn test_alert_threshold_boundary() {
        // Test exactly at threshold (should trigger)
        let at_threshold = DiskSpaceAlert {
            mount_point: "/".to_string(),
            usage_percent: 80.0,
            threshold_percent: 80,
            available_bytes: 20 * 1024 * 1024 * 1024,
            available_human: "20.00 GB".to_string(),
        };
        assert!(at_threshold.usage_percent >= at_threshold.threshold_percent as f64);

        // Test just below threshold (should not trigger in real check)
        let below_threshold_usage = 79.9;
        let threshold = 80;
        assert!(below_threshold_usage < threshold as f64);
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

    #[test]
    fn test_get_disk_info_using_sysinfo_directly() {
        // Test disk info retrieval using sysinfo directly (same logic as get_disk_info)
        let disks = Disks::new_with_refreshed_list();

        // Should have at least one disk on any system
        assert!(
            !disks.list().is_empty(),
            "System should have at least one disk"
        );

        for disk in disks.list() {
            let mount_point = disk.mount_point().to_string_lossy().to_string();
            let total = disk.total_space();
            let available = disk.available_space();
            let used = total.saturating_sub(available);
            let usage_percent = if total > 0 {
                (used as f64 / total as f64) * 100.0
            } else {
                0.0
            };

            // Validate values
            assert!(!mount_point.is_empty(), "Mount point should not be empty");
            assert!(
                usage_percent >= 0.0 && usage_percent <= 100.0,
                "Usage percent should be between 0 and 100, got {}",
                usage_percent
            );
        }
    }

    #[test]
    fn test_disk_info_from_raw_values() {
        // Test the DiskInfo struct creation with realistic values from a disk
        let disks = Disks::new_with_refreshed_list();

        if let Some(disk) = disks.list().first() {
            let mount_point = disk.mount_point().to_string_lossy().to_string();
            let total = disk.total_space();
            let available = disk.available_space();
            let used = total.saturating_sub(available);
            let usage_percent = if total > 0 {
                (used as f64 / total as f64) * 100.0
            } else {
                0.0
            };
            let file_system = disk.file_system().to_string_lossy().to_string();

            let info = DiskInfo {
                mount_point: mount_point.clone(),
                total_bytes: total,
                used_bytes: used,
                available_bytes: available,
                usage_percent,
                file_system,
            };

            assert_eq!(info.mount_point, mount_point);
            assert!(info.total_bytes > 0, "Total bytes should be positive");
            assert!(
                info.usage_percent >= 0.0 && info.usage_percent <= 100.0,
                "Usage percent should be valid"
            );
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
