//! Disk status collection
//!
//! Pure, read-only disk-usage inspection shared between the on-demand HTTP
//! endpoint (Settings API) and the background `DiskSpaceMonitor` in
//! `temps-monitoring`. Reading disk usage needs only the configured threshold
//! (from `ConfigService`) — it never sends notifications, so it has no
//! dependency on the notification service.
//!
//! By default every writable mounted volume is monitored (e.g. a dedicated
//! `/var/lib/docker` volume), not just the disk backing the data directory.
//! Setting `disk_space_alert.monitor_path` restricts monitoring to the single
//! disk backing that path.

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

/// Raw view of a mounted disk as reported by the OS, before any filtering.
/// Kept separate from sysinfo so the selection/dedup logic below is
/// unit-testable without real mounts.
#[derive(Debug, Clone)]
struct RawDisk {
    device: String,
    mount_point: String,
    total: u64,
    available: u64,
    file_system: String,
    read_only: bool,
}

/// File systems that never hold data Temps could fill up and would only
/// produce false alerts (snap squashfs loops, for example, are permanently
/// at 100% usage).
const PSEUDO_FILE_SYSTEMS: &[&str] = &[
    "squashfs", "erofs", "iso9660", "overlay", "tmpfs", "devtmpfs", "ramfs",
];

fn collect_raw_disks() -> Vec<RawDisk> {
    Disks::new_with_refreshed_list()
        .list()
        .iter()
        .map(|disk| RawDisk {
            device: disk.name().to_string_lossy().to_string(),
            mount_point: disk.mount_point().to_string_lossy().to_string(),
            total: disk.total_space(),
            available: disk.available_space(),
            file_system: disk.file_system().to_string_lossy().to_string(),
            read_only: disk.is_read_only(),
        })
        .collect()
}

fn to_disk_info(raw: &RawDisk) -> DiskInfo {
    let used = raw.total.saturating_sub(raw.available);
    let usage_percent = if raw.total > 0 {
        (used as f64 / raw.total as f64) * 100.0
    } else {
        0.0
    };
    DiskInfo {
        mount_point: raw.mount_point.clone(),
        total_bytes: raw.total,
        used_bytes: used,
        available_bytes: raw.available,
        usage_percent,
        file_system: raw.file_system.clone(),
    }
}

/// Inspect disk usage for all mounted volumes, or only the mount backing
/// `path`.
///
/// When `path` is given, the most specific matching mount point is returned
/// (longest mount-point prefix wins). Without a path, every writable mounted
/// volume is returned — deduplicated per device and with pseudo file systems
/// filtered out — so additional volumes (a dedicated `/var/lib/docker`
/// mount, an attached data disk) are all covered.
pub fn get_disk_info(path: Option<&str>) -> Vec<DiskInfo> {
    build_disk_infos(&collect_raw_disks(), path)
}

fn build_disk_infos(raw_disks: &[RawDisk], path: Option<&str>) -> Vec<DiskInfo> {
    if let Some(target_path) = path {
        // Keep only mounts that could contain the target path — the root
        // mount ("/") is always a candidate as a fallback — then pick the
        // most specific one (longest mount point).
        let mut candidates: Vec<&RawDisk> = raw_disks
            .iter()
            .filter(|d| target_path.starts_with(&d.mount_point) || d.mount_point == "/")
            .collect();
        candidates.sort_by_key(|d| std::cmp::Reverse(d.mount_point.len()));
        return candidates.into_iter().take(1).map(to_disk_info).collect();
    }

    // All-disks mode: monitor every real, writable volume. Bind mounts and
    // btrfs subvolumes surface the same device several times, so keep only
    // the shortest (primary) mount point per device.
    let mut by_device: std::collections::BTreeMap<String, &RawDisk> =
        std::collections::BTreeMap::new();
    for disk in raw_disks {
        if disk.total == 0
            || disk.read_only
            || PSEUDO_FILE_SYSTEMS.contains(&disk.file_system.to_ascii_lowercase().as_str())
        {
            continue;
        }
        let key = if disk.device.is_empty() {
            format!("mount:{}", disk.mount_point)
        } else {
            disk.device.clone()
        };
        match by_device.get(key.as_str()) {
            Some(existing) if existing.mount_point.len() <= disk.mount_point.len() => {}
            _ => {
                by_device.insert(key, disk);
            }
        }
    }

    let mut disk_infos: Vec<DiskInfo> = by_device.values().map(|d| to_disk_info(d)).collect();
    disk_infos.sort_by(|a, b| a.mount_point.cmp(&b.mount_point));
    disk_infos
}

/// Collect the current disk status, evaluating every monitored disk against
/// the configured threshold. Read-only — never sends notifications.
///
/// With no `monitor_path` configured (the default), all mounted writable
/// volumes are checked; a `monitor_path` restricts the check to the single
/// disk backing that path.
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

    let disks = get_disk_info(settings.monitor_path.as_deref());

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

    fn raw(device: &str, mount: &str, total: u64, available: u64) -> RawDisk {
        RawDisk {
            device: device.to_string(),
            mount_point: mount.to_string(),
            total,
            available,
            file_system: "ext4".to_string(),
            read_only: false,
        }
    }

    const GB: u64 = 1024 * 1024 * 1024;

    #[test]
    fn test_all_disks_mode_includes_every_writable_volume() {
        let disks = vec![
            raw("/dev/sda1", "/", 40 * GB, 30 * GB),
            raw("/dev/sdb1", "/var/lib/docker", 500 * GB, 100 * GB),
            raw("/dev/sdc1", "/mnt/data", 100 * GB, 90 * GB),
        ];

        let infos = build_disk_infos(&disks, None);

        let mounts: Vec<&str> = infos.iter().map(|d| d.mount_point.as_str()).collect();
        assert_eq!(mounts, vec!["/", "/mnt/data", "/var/lib/docker"]);
        let docker = infos.iter().find(|d| d.mount_point == "/var/lib/docker");
        assert_eq!(docker.unwrap().usage_percent, 80.0);
    }

    #[test]
    fn test_all_disks_mode_dedupes_bind_mounts_by_device() {
        // Same device mounted twice (bind mount / btrfs subvolume): keep the
        // primary (shortest) mount point only.
        let disks = vec![
            raw("/dev/sda1", "/home/user/data", 40 * GB, 30 * GB),
            raw("/dev/sda1", "/", 40 * GB, 30 * GB),
        ];

        let infos = build_disk_infos(&disks, None);

        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].mount_point, "/");
    }

    #[test]
    fn test_all_disks_mode_skips_pseudo_and_readonly_mounts() {
        let mut snap = raw("/dev/loop0", "/snap/core/1234", GB, 0);
        snap.file_system = "squashfs".to_string();
        snap.read_only = true;
        let mut cdrom = raw("/dev/sr0", "/media/cdrom", GB, 0);
        cdrom.read_only = true;
        let empty = raw("/dev/sdz1", "/mnt/empty", 0, 0);
        let disks = vec![raw("/dev/sda1", "/", 40 * GB, 30 * GB), snap, cdrom, empty];

        let infos = build_disk_infos(&disks, None);

        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].mount_point, "/");
    }

    #[test]
    fn test_path_mode_picks_most_specific_mount() {
        let disks = vec![
            raw("/dev/sda1", "/", 40 * GB, 30 * GB),
            raw("/dev/sdb1", "/var/lib/docker", 500 * GB, 100 * GB),
        ];

        let infos = build_disk_infos(&disks, Some("/var/lib/docker/volumes"));
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].mount_point, "/var/lib/docker");

        // A path on no dedicated mount falls back to the root disk.
        let infos = build_disk_infos(&disks, Some("/opt/temps"));
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].mount_point, "/");
    }
}
