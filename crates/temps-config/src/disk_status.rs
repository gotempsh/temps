//! Disk status collection
//!
//! Pure, read-only disk-usage inspection shared between the on-demand HTTP
//! endpoint (Settings API) and the background `DiskSpaceMonitor` in
//! `temps-monitoring`. Reading disk usage needs only the configured threshold
//! (from `ConfigService`) and the data directory — it never sends
//! notifications, so it has no dependency on the notification service.

use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sysinfo::Disks;
use thiserror::Error;
use utoipa::ToSchema;

use crate::ConfigService;

/// Disk space information for a single disk/partition
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
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

/// Alert for a disk that exceeds the threshold
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
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

/// Result of a disk space check
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DiskSpaceCheckResult {
    /// Timestamp of the check (ISO 8601, UTC)
    #[schema(value_type = String, format = DateTime, example = "2026-05-28T12:15:47.609192Z")]
    pub checked_at: DateTime<Utc>,
    /// Whether disk space monitoring is enabled in settings
    pub enabled: bool,
    /// Configured alert threshold percentage (0-100)
    pub threshold_percent: u32,
    /// List of all monitored disks
    pub disks: Vec<DiskInfo>,
    /// Disks that meet or exceed the threshold
    pub alerts: Vec<DiskSpaceAlert>,
}

#[derive(Debug, Error)]
pub enum DiskStatusError {
    #[error("Failed to load disk-space settings: {reason}")]
    Configuration { reason: String },
}

/// Inspect disk usage for all mounts, or only the mount backing `path`.
///
/// When `path` is given, the most specific matching mount point is returned
/// (longest mount-point prefix wins).
pub fn get_disk_info(path: Option<&str>) -> Vec<DiskInfo> {
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

        if let Some(target_path) = path {
            // Keep only mounts that could contain the target path. The root
            // mount ("/") is always a candidate as a fallback.
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

    // For a specific path with multiple candidate mounts, keep the most
    // specific one (longest mount point).
    if path.is_some() && disk_infos.len() > 1 {
        disk_infos.sort_by_key(|d| std::cmp::Reverse(d.mount_point.len()));
        disk_infos.truncate(1);
    }

    disk_infos
}

/// Collect the current disk status for the monitored path, evaluating it
/// against the configured threshold. Read-only — never sends notifications.
pub async fn collect_disk_status(
    config_service: &ConfigService,
) -> Result<DiskSpaceCheckResult, DiskStatusError> {
    let settings = config_service
        .get_settings()
        .await
        .map_err(|e| DiskStatusError::Configuration {
            reason: e.to_string(),
        })?
        .disk_space_alert;

    let data_dir = config_service.data_dir();
    let monitor_path = settings
        .monitor_path
        .clone()
        .unwrap_or_else(|| data_dir.to_string_lossy().to_string());

    let disks = get_disk_info(Some(&monitor_path));

    let alerts = disks
        .iter()
        .filter(|disk| disk.usage_percent >= settings.threshold_percent as f64)
        .map(|disk| DiskSpaceAlert {
            mount_point: disk.mount_point.clone(),
            usage_percent: disk.usage_percent,
            threshold_percent: settings.threshold_percent,
            available_bytes: disk.available_bytes,
            available_human: format_bytes(disk.available_bytes),
        })
        .collect();

    Ok(DiskSpaceCheckResult {
        checked_at: Utc::now(),
        enabled: settings.enabled,
        threshold_percent: settings.threshold_percent,
        disks,
        alerts,
    })
}

/// Convenience: resolve the most specific disk backing `path` directly.
pub fn disk_for_path(path: &Path) -> Option<DiskInfo> {
    get_disk_info(Some(&path.to_string_lossy()))
        .into_iter()
        .next()
}

/// Format bytes into a human-readable string (binary units).
pub fn format_bytes(bytes: u64) -> String {
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
        assert_eq!(format_bytes(1023), "1023 bytes");
        assert_eq!(format_bytes(1024 * 1024 - 1), "1024.00 KB");
        let five_tb = 5 * 1024 * 1024 * 1024 * 1024u64;
        assert_eq!(format_bytes(five_tb), "5.00 TB");
    }

    #[test]
    fn test_get_disk_info_returns_valid_values() {
        let disks = get_disk_info(None);
        assert!(!disks.is_empty(), "System should report at least one disk");
        for disk in &disks {
            assert!(!disk.mount_point.is_empty());
            assert!(
                (0.0..=100.0).contains(&disk.usage_percent),
                "usage_percent out of range: {}",
                disk.usage_percent
            );
            assert_eq!(
                disk.total_bytes,
                disk.used_bytes + disk.available_bytes,
                "total should equal used + available"
            );
        }
    }

    #[test]
    fn test_get_disk_info_for_path_returns_single_mount() {
        // Any absolute path should resolve to exactly one backing mount.
        let disks = get_disk_info(Some("/"));
        assert!(
            disks.len() <= 1,
            "path query should collapse to a single mount, got {}",
            disks.len()
        );
    }
}
