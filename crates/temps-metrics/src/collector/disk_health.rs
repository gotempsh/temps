// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Disk-array and drive health for the node collector.
//!
//! A software RAID array can run degraded on a dead member indefinitely, and
//! an SSD under a ClickHouse/Postgres write load can wear out within a few
//! years, without any of the usage metrics moving. This module reports both.
//!
//! ## Sources
//!
//! - **RAID**: `/proc/mdstat`, read on every scrape (one small procfs read).
//!   An array is degraded when its `[n/m]` status shows fewer active members
//!   than configured. Arrays without that status (RAID 0, linear, inactive
//!   IMSM/DDF containers) are listed but never reported degraded: they
//!   have no redundancy to lose, and containers are inactive by design.
//! - **NVMe**: the SMART / Health Information log page (log id `0x02`) read
//!   with the `NVME_IOCTL_ADMIN_CMD` ioctl on the controller character device
//!   (`/dev/nvme0`). No extra tools; needs read access to the device, which
//!   in practice means running as root.
//! - **SATA/SAS** (`/dev/sd*`): `smartctl -j` when smartmontools is installed
//!   (there is no tool-free equivalent of the NVMe log page for ATA). Also
//!   used for an NVMe controller whose ioctl is refused.
//!
//! SMART data changes slowly and reading it is not free (smartctl is a
//! process per drive), so drive health is refreshed every
//! [`DRIVE_REFRESH_INTERVAL`] and the cached values are re-emitted on the
//! scrapes in between. RAID state is read fresh every scrape. Either way a
//! failure surfaces well within the five minutes the default alert rules
//! allow.
//!
//! ## Metrics
//!
//! Each metric has one unlabelled aggregate series (what alert rules and the
//! headline tiles read: the store prefers the series with the fewest label
//! keys) plus one labelled series per array (`array`) or drive (`device`)
//! for the breakdown table.
//!
//! | Name | Aggregate | Per item |
//! |---|---|---|
//! | `node.raid_arrays` | arrays in `/proc/mdstat` | — |
//! | `node.raid_degraded` | degraded arrays | 0/1 per `array` |
//! | `node.raid_devices_failed` | missing members, all arrays | per `array` |
//! | `node.disk_health_drives` | drives whose health could be read | — |
//! | `node.disk_health_drives_detected` | physical drives found | — |
//! | `node.disk_wear_percent` | worst drive | NVMe "Percentage Used" / SATA wear |
//! | `node.disk_media_errors` | sum | NVMe media errors / reallocated + pending sectors |
//! | `node.disk_bytes_written_total` | sum | lifetime host writes (cumulative) |

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use tracing::{debug, info};

use super::CollectorConfig;
use crate::store::{MetricKind, MetricPoint, SourceKind};

/// How often SMART data is re-read; cached values are re-emitted in between.
pub const DRIVE_REFRESH_INTERVAL: Duration = Duration::from_secs(120);

/// Longest one `smartctl` invocation may take.
const SMARTCTL_TIMEOUT: Duration = Duration::from_secs(15);

/// Drives probed per refresh at most, so a JBOD with hundreds of disks can't
/// turn one scrape into minutes of smartctl calls.
const MAX_DRIVES: usize = 64;

// ── RAID (/proc/mdstat) ───────────────────────────────────────────────────────

/// One md array from `/proc/mdstat`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RaidArray {
    /// `md0`, `md127`, …
    pub name: String,
    /// `raid1`, `raid5`, … or `None` for an inactive array.
    pub level: Option<String>,
    /// Configured members and active members, from `[n/m]`. `None` for
    /// levels without redundancy and inactive containers.
    pub members: Option<(u32, u32)>,
    /// Members marked failed: `(F)`.
    pub failed_marked: u32,
}

impl RaidArray {
    pub fn degraded(&self) -> bool {
        matches!(self.members, Some((total, active)) if active < total)
    }

    /// Members missing from the array (failed or removed).
    pub fn devices_failed(&self) -> u32 {
        match self.members {
            Some((total, active)) => total.saturating_sub(active).max(self.failed_marked),
            None => self.failed_marked,
        }
    }
}

/// Parse `/proc/mdstat`.
///
/// ```text
/// md0 : active raid1 sdb1[1] sda1[0](F)
///       1046528 blocks super 1.2 [2/1] [_U]
/// ```
pub fn parse_mdstat(content: &str) -> Vec<RaidArray> {
    let mut arrays: Vec<RaidArray> = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some((name, rest)) = trimmed.split_once(" : ") {
            let name = name.trim();
            if !name.starts_with("md") || name.contains(' ') {
                continue;
            }
            let mut words = rest.split_whitespace();
            let state = words.next().unwrap_or_default();
            let mut level = None;
            let mut failed_marked = 0;
            for word in words {
                if word.starts_with('(') {
                    // `(auto-read-only)`, `(read-only)`
                    continue;
                }
                if level.is_none()
                    && state == "active"
                    && !word.contains('[')
                    && (word.starts_with("raid") || word == "linear" || word == "multipath")
                {
                    level = Some(word.to_string());
                    continue;
                }
                if word.ends_with("(F)") {
                    failed_marked += 1;
                }
            }
            arrays.push(RaidArray {
                name: name.to_string(),
                level,
                members: None,
                failed_marked,
            });
            continue;
        }
        // Status line of the most recent array: `... [2/1] [_U]`.
        if let Some(current) = arrays.last_mut() {
            if current.members.is_none() {
                current.members = parse_member_counts(trimmed);
            }
        }
    }
    arrays
}

/// The `[n/m]` token of an mdstat status line.
fn parse_member_counts(line: &str) -> Option<(u32, u32)> {
    line.split_whitespace().find_map(|word| {
        let inner = word.strip_prefix('[')?.strip_suffix(']')?;
        let (total, active) = inner.split_once('/')?;
        Some((total.parse().ok()?, active.parse().ok()?))
    })
}

// ── Drives ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriveKind {
    /// An NVMe controller (`/dev/nvme0`).
    Nvme,
    /// A SCSI-attached disk (`/dev/sda`): SATA, SAS or USB.
    Scsi,
}

/// A physical drive to read health from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Drive {
    /// Label value: `nvme0`, `sda`.
    pub name: String,
    pub kind: DriveKind,
}

impl Drive {
    fn dev_path(&self) -> String {
        format!("/dev/{}", self.name)
    }
}

/// Health of one drive. A field is `None` when the drive does not report it.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct DriveHealth {
    pub device: String,
    /// Share of rated endurance used, 0–100 (NVMe may report up to 255).
    pub wear_percent: Option<f64>,
    /// NVMe media and data integrity errors, or reallocated + pending
    /// sectors on ATA.
    pub media_errors: Option<u64>,
    /// Lifetime bytes written by the host.
    pub bytes_written: Option<u64>,
}

/// Physical drives under `sys_block` (normally `/sys/block`): NVMe
/// controllers (deduplicated from their namespaces) and non-removable
/// `sd*` disks. Virtual devices (`vd*`, `xvd*`, loop, dm, md, zram) have no
/// SMART data and are skipped.
pub fn list_drives(sys_block: &Path) -> Vec<Drive> {
    let Ok(entries) = std::fs::read_dir(sys_block) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    let mut drives: Vec<Drive> = Vec::new();
    for name in names {
        if let Some(controller) = nvme_controller_of(&name) {
            if !drives.iter().any(|d| d.name == controller) {
                drives.push(Drive {
                    name: controller,
                    kind: DriveKind::Nvme,
                });
            }
        } else if is_scsi_disk(&name) {
            let removable = std::fs::read_to_string(sys_block.join(&name).join("removable"))
                .map(|s| s.trim() == "1")
                .unwrap_or(false);
            if !removable {
                drives.push(Drive {
                    name,
                    kind: DriveKind::Scsi,
                });
            }
        }
    }
    drives.truncate(MAX_DRIVES);
    drives
}

/// `nvme0n1` → `nvme0`; `nvme0c0n1` (multipath path) → `nvme0`.
fn nvme_controller_of(block_name: &str) -> Option<String> {
    let rest = block_name.strip_prefix("nvme")?;
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    let after = &rest[digits.len()..];
    if digits.is_empty() || !(after.starts_with('n') || after.starts_with('c')) {
        return None;
    }
    Some(format!("nvme{digits}"))
}

/// `sda`, `sdab` — a whole SCSI disk, not a partition.
fn is_scsi_disk(name: &str) -> bool {
    name.strip_prefix("sd")
        .is_some_and(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_lowercase()))
}

/// Size of the NVMe SMART / Health Information log page.
pub const NVME_SMART_LOG_LEN: usize = 512;

/// Bytes in one NVMe "data unit" (1000 × 512-byte units).
const NVME_DATA_UNIT_BYTES: u128 = 512_000;

/// Little-endian 128-bit counter at `offset`.
fn le_u128(log: &[u8], offset: usize) -> u128 {
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&log[offset..offset + 16]);
    u128::from_le_bytes(bytes)
}

/// Decode the fields we use from an NVMe SMART / Health Information log
/// page (NVM Express Base Specification, "SMART / Health Information").
pub fn parse_nvme_smart_log(device: &str, log: &[u8; NVME_SMART_LOG_LEN]) -> DriveHealth {
    let percentage_used = log[5];
    let data_units_written = le_u128(log, 48);
    let media_errors = le_u128(log, 160);
    DriveHealth {
        device: device.to_string(),
        wear_percent: Some(f64::from(percentage_used)),
        media_errors: Some(u64::try_from(media_errors).unwrap_or(u64::MAX)),
        bytes_written: Some(
            u64::try_from(data_units_written.saturating_mul(NVME_DATA_UNIT_BYTES))
                .unwrap_or(u64::MAX),
        ),
    }
}

/// Read the SMART log page from an NVMe controller character device.
#[cfg(target_os = "linux")]
fn read_nvme_smart_log(dev_path: &str) -> std::io::Result<[u8; NVME_SMART_LOG_LEN]> {
    use std::os::fd::AsRawFd;

    /// `struct nvme_passthru_cmd` from `<linux/nvme_ioctl.h>`.
    #[repr(C)]
    #[derive(Default)]
    struct NvmePassthruCmd {
        opcode: u8,
        flags: u8,
        rsvd1: u16,
        nsid: u32,
        cdw2: u32,
        cdw3: u32,
        metadata: u64,
        addr: u64,
        metadata_len: u32,
        data_len: u32,
        cdw10: u32,
        cdw11: u32,
        cdw12: u32,
        cdw13: u32,
        cdw14: u32,
        cdw15: u32,
        timeout_ms: u32,
        result: u32,
    }
    const _: () = assert!(std::mem::size_of::<NvmePassthruCmd>() == 72);
    /// `_IOWR('N', 0x41, struct nvme_admin_cmd)`.
    const NVME_IOCTL_ADMIN_CMD: u32 = 0xC048_4E41;
    const NVME_ADMIN_GET_LOG_PAGE: u8 = 0x02;
    const NVME_LOG_SMART: u32 = 0x02;
    const NVME_NSID_ALL: u32 = 0xFFFF_FFFF;

    let file = std::fs::File::open(dev_path)?;
    let mut log = [0u8; NVME_SMART_LOG_LEN];
    let dwords = (NVME_SMART_LOG_LEN / 4) as u32;
    let mut cmd = NvmePassthruCmd {
        opcode: NVME_ADMIN_GET_LOG_PAGE,
        nsid: NVME_NSID_ALL,
        addr: log.as_mut_ptr() as u64,
        data_len: NVME_SMART_LOG_LEN as u32,
        // NUMDL (dwords - 1) in bits 31:16, log id in bits 7:0.
        cdw10: ((dwords - 1) << 16) | NVME_LOG_SMART,
        timeout_ms: 5_000,
        ..Default::default()
    };
    // SAFETY: `cmd` is a correctly laid out `nvme_passthru_cmd` (size
    // asserted above) whose `addr`/`data_len` describe `log`, a live
    // 512-byte buffer that outlives the call; the kernel writes at most
    // `data_len` bytes into it. The fd is open for the duration of the call.
    let rc = unsafe { libc::ioctl(file.as_raw_fd(), NVME_IOCTL_ADMIN_CMD as _, &mut cmd) };
    if rc < 0 {
        return Err(std::io::Error::last_os_error());
    }
    if rc > 0 {
        // A positive return is the NVMe status code of a failed command.
        return Err(std::io::Error::other(format!(
            "NVMe Get Log Page (SMART) failed with status {rc:#x}"
        )));
    }
    Ok(log)
}

#[cfg(not(target_os = "linux"))]
fn read_nvme_smart_log(_dev_path: &str) -> std::io::Result<[u8; NVME_SMART_LOG_LEN]> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "NVMe SMART ioctl is Linux-only",
    ))
}

/// ATA attributes whose normalized value counts down remaining life, most
/// specific first: SSD_Life_Left, Media_Wearout_Indicator,
/// Wear_Leveling_Count, Percent_Lifetime_Remain.
const ATA_LIFE_LEFT_ATTRIBUTES: [u64; 4] = [231, 233, 177, 202];
/// Reallocated_Sector_Ct, Current_Pending_Sector.
const ATA_MEDIA_ERROR_ATTRIBUTES: [u64; 2] = [5, 197];
/// Total_LBAs_Written.
const ATA_LBAS_WRITTEN: u64 = 241;

/// Decode `smartctl -j -A -i -l error -l ssd` output for an ATA, NVMe or
/// SCSI/SAS drive. `None` when it holds no health data at all (unsupported
/// device, standby, permission denied).
pub fn parse_smartctl_json(device: &str, json: &serde_json::Value) -> Option<DriveHealth> {
    if let Some(nvme) = json.get("nvme_smart_health_information_log") {
        let units = nvme.get("data_units_written").and_then(|v| v.as_u64());
        return Some(DriveHealth {
            device: device.to_string(),
            wear_percent: nvme.get("percentage_used").and_then(|v| v.as_f64()),
            media_errors: nvme.get("media_errors").and_then(|v| v.as_u64()),
            bytes_written: units.map(|u| u.saturating_mul(NVME_DATA_UNIT_BYTES as u64)),
        });
    }

    let Some(table) = json
        .get("ata_smart_attributes")
        .and_then(|a| a.get("table"))
        .and_then(|t| t.as_array())
    else {
        // SAS/SCSI drives have no ATA attribute table.
        return parse_scsi_smartctl_json(device, json);
    };
    let attr = |id: u64| {
        table
            .iter()
            .find(|row| row.get("id").and_then(|v| v.as_u64()) == Some(id))
    };
    let raw = |id: u64| {
        attr(id)
            .and_then(|row| row.get("raw"))
            .and_then(|r| r.get("value"))
            .and_then(|v| v.as_u64())
    };

    // smartctl ≥ 7.3 reports the device-statistics endurance figure directly.
    let wear_percent = json
        .get("endurance_used")
        .and_then(|e| e.get("current_percent"))
        .and_then(|v| v.as_f64())
        .or_else(|| {
            ATA_LIFE_LEFT_ATTRIBUTES.iter().find_map(|&id| {
                let value = attr(id)?.get("value")?.as_f64()?;
                Some((100.0 - value).clamp(0.0, 100.0))
            })
        });
    let media_errors = {
        let present: Vec<u64> = ATA_MEDIA_ERROR_ATTRIBUTES
            .iter()
            .filter_map(|&id| raw(id))
            .collect();
        (!present.is_empty()).then(|| present.iter().sum())
    };
    let block_size = json
        .get("logical_block_size")
        .and_then(|v| v.as_u64())
        .unwrap_or(512);
    let bytes_written = raw(ATA_LBAS_WRITTEN).map(|lbas| lbas.saturating_mul(block_size));

    Some(DriveHealth {
        device: device.to_string(),
        wear_percent,
        media_errors,
        bytes_written,
    })
}

/// smartctl's "gigabytes" in the SCSI error counter log are 10^9 bytes.
const SCSI_GIGABYTE_BYTES: f64 = 1_000_000_000.0;

/// The SCSI/SAS part of [`parse_smartctl_json`]: endurance from the SSD log
/// page (`-l ssd`), media errors as the grown defect list plus the
/// uncorrected read/write/verify errors (`-l error`), and bytes written from
/// the write error counter. `None` when none of them is present.
fn parse_scsi_smartctl_json(device: &str, json: &serde_json::Value) -> Option<DriveHealth> {
    let wear_percent = json
        .get("scsi_percentage_used_endurance_indicator")
        .and_then(|v| v.as_f64())
        .or_else(|| {
            json.get("endurance_used")
                .and_then(|e| e.get("current_percent"))
                .and_then(|v| v.as_f64())
        });

    let counters = json.get("scsi_error_counter_log");
    let counter = |direction: &str| counters.and_then(|c| c.get(direction));
    let media_errors = {
        let present: Vec<u64> = ["read", "write", "verify"]
            .iter()
            .filter_map(|direction| {
                counter(direction)?
                    .get("total_uncorrected_errors")?
                    .as_u64()
            })
            .chain(json.get("scsi_grown_defect_list").and_then(|v| v.as_u64()))
            .collect();
        (!present.is_empty()).then(|| present.iter().fold(0u64, |a, b| a.saturating_add(*b)))
    };
    // A decimal string in smartctl's JSON ("1234.567"); accept a number too.
    let bytes_written = counter("write")
        .and_then(|w| w.get("gigabytes_processed"))
        .and_then(|v| {
            v.as_f64()
                .or_else(|| v.as_str().and_then(|s| s.trim().parse::<f64>().ok()))
        })
        .filter(|gb| gb.is_finite() && *gb >= 0.0)
        .map(|gb| (gb * SCSI_GIGABYTE_BYTES) as u64);

    if wear_percent.is_none() && media_errors.is_none() && bytes_written.is_none() {
        return None;
    }
    Some(DriveHealth {
        device: device.to_string(),
        wear_percent,
        media_errors,
        bytes_written,
    })
}

/// Outcome of trying `smartctl` once.
enum Smartctl {
    Health(DriveHealth),
    /// It ran but reported no health data for this drive.
    NoData,
    /// The binary is not installed.
    Missing,
}

async fn run_smartctl(drive: &Drive) -> Smartctl {
    let mut cmd = tokio::process::Command::new("smartctl");
    // `-n standby`: never spin up a sleeping disk just to read it. A SAS/SCSI
    // drive reports its uncorrected errors only in the error counter log
    // (`-l error`) and its endurance only in the SSD page (`-l ssd`); ATA and
    // NVMe drives just return a little more JSON for them.
    cmd.args([
        "-j",
        "-A",
        "-i",
        "-l",
        "error",
        "-l",
        "ssd",
        "-n",
        "standby",
        &drive.dev_path(),
    ])
    .stdin(std::process::Stdio::null())
    .stderr(std::process::Stdio::null())
    .kill_on_drop(true);
    let output = match tokio::time::timeout(SMARTCTL_TIMEOUT, cmd.output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => return Smartctl::Missing,
        Ok(Err(e)) => {
            debug!(device = %drive.name, error = %e, "smartctl could not be started");
            return Smartctl::NoData;
        }
        Err(_) => {
            debug!(device = %drive.name, "smartctl timed out");
            return Smartctl::NoData;
        }
    };
    // smartctl's exit status is a bitmask in which most bits report the
    // drive's condition (e.g. "SMART says failing") rather than an error, so
    // the JSON is parsed whatever the status.
    match serde_json::from_slice::<serde_json::Value>(&output.stdout) {
        Ok(json) => match parse_smartctl_json(&drive.name, &json) {
            Some(health) => Smartctl::Health(health),
            None => Smartctl::NoData,
        },
        Err(_) => Smartctl::NoData,
    }
}

/// Last drive-health reading. Refreshed in a background task every
/// [`DRIVE_REFRESH_INTERVAL`], so a slow or hung `smartctl` never delays a
/// node sample: the sample just re-emits the previous reading.
#[derive(Debug)]
pub struct DriveHealthCache {
    sys_block: PathBuf,
    state: Mutex<CacheState>,
    refreshing: AtomicBool,
}

#[derive(Debug, Default)]
struct CacheState {
    refreshed_at: Option<Instant>,
    /// `(drives detected, drives read)` from the last completed refresh.
    reading: Option<(usize, Vec<DriveHealth>)>,
    smartctl_missing_logged: bool,
}

impl DriveHealthCache {
    /// `sys_block` is normally `/sys/block`.
    pub fn new(sys_block: impl Into<PathBuf>) -> Arc<Self> {
        Arc::new(Self {
            sys_block: sys_block.into(),
            state: Mutex::new(CacheState::default()),
            refreshing: AtomicBool::new(false),
        })
    }

    fn due(&self, now: Instant) -> bool {
        self.state
            .lock()
            .map(|s| {
                s.refreshed_at
                    .is_none_or(|t| now.duration_since(t) >= DRIVE_REFRESH_INTERVAL)
            })
            .unwrap_or(false)
    }

    /// The last completed reading (`None` until the first one finishes),
    /// starting a background refresh when one is due. Must be called from
    /// within a Tokio runtime.
    pub fn current(self: &Arc<Self>) -> Option<(usize, Vec<DriveHealth>)> {
        if self.due(Instant::now())
            && self
                .refreshing
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
        {
            let cache = Arc::clone(self);
            tokio::spawn(async move {
                cache.refresh().await;
                cache.refreshing.store(false, Ordering::Release);
            });
        }
        self.state.lock().ok().and_then(|s| s.reading.clone())
    }

    /// Read every drive now and store the result.
    pub async fn refresh(&self) {
        let drives = list_drives(&self.sys_block);
        let (health, smartctl_missing) = read_drives(&drives).await;
        if let Ok(mut state) = self.state.lock() {
            if smartctl_missing
                && !state.smartctl_missing_logged
                && drives.iter().any(|d| d.kind == DriveKind::Scsi)
            {
                info!(
                    "SATA/SAS drive health (wear, reallocated sectors) needs smartmontools: \
                     install it (`smartctl`) to report it; NVMe drives are read without it"
                );
                state.smartctl_missing_logged = true;
            }
            state.refreshed_at = Some(Instant::now());
            state.reading = Some((drives.len(), health));
        }
    }
}

/// Read every drive; the bool is whether `smartctl` is missing.
async fn read_drives(drives: &[Drive]) -> (Vec<DriveHealth>, bool) {
    let mut out = Vec::new();
    let mut smartctl_missing = false;
    for drive in drives {
        if drive.kind == DriveKind::Nvme {
            let path = drive.dev_path();
            let read = tokio::task::spawn_blocking(move || read_nvme_smart_log(&path)).await;
            match read {
                Ok(Ok(log)) => {
                    out.push(parse_nvme_smart_log(&drive.name, &log));
                    continue;
                }
                Ok(Err(e)) => debug!(
                    device = %drive.name,
                    error = %e,
                    "NVMe SMART log not readable via ioctl; trying smartctl"
                ),
                Err(e) => debug!(device = %drive.name, error = %e, "NVMe SMART read task failed"),
            }
        }
        if smartctl_missing {
            continue;
        }
        match run_smartctl(drive).await {
            Smartctl::Health(h) => out.push(h),
            Smartctl::NoData => {}
            Smartctl::Missing => smartctl_missing = true,
        }
    }
    (out, smartctl_missing)
}

// ── Metric points ─────────────────────────────────────────────────────────────

fn point(
    config: &CollectorConfig,
    name: &str,
    value: f64,
    time: DateTime<Utc>,
    label: Option<(&str, &str)>,
) -> MetricPoint {
    let mut labels = HashMap::new();
    if let Some((k, v)) = label {
        labels.insert(k.to_string(), v.to_string());
    }
    MetricPoint {
        time,
        source_kind: SourceKind::Node,
        source_id: config.source_id,
        name: name.to_string(),
        value,
        kind: MetricKind::Gauge,
        engine: None,
        environment: config.environment.clone(),
        node_id: config.node_id,
        labels,
    }
}

/// Points for the RAID arrays in `/proc/mdstat`. An empty list still emits
/// `node.raid_arrays = 0` and `node.raid_degraded = 0`, so "no software
/// RAID" is distinguishable from "not reported".
pub fn raid_points(
    config: &CollectorConfig,
    arrays: &[RaidArray],
    now: DateTime<Utc>,
) -> Vec<MetricPoint> {
    let degraded = arrays.iter().filter(|a| a.degraded()).count();
    let failed: u32 = arrays.iter().map(|a| a.devices_failed()).sum();
    let mut points = vec![
        point(config, "node.raid_arrays", arrays.len() as f64, now, None),
        point(config, "node.raid_degraded", degraded as f64, now, None),
        point(
            config,
            "node.raid_devices_failed",
            f64::from(failed),
            now,
            None,
        ),
    ];
    for a in arrays {
        let label = Some(("array", a.name.as_str()));
        points.push(point(
            config,
            "node.raid_degraded",
            if a.degraded() { 1.0 } else { 0.0 },
            now,
            label,
        ));
        points.push(point(
            config,
            "node.raid_devices_failed",
            f64::from(a.devices_failed()),
            now,
            label,
        ));
    }
    points
}

/// Points for drive health. Aggregates are only emitted when at least one
/// drive reported the field, so a host with no readable drive gets no wear
/// value at all rather than a reassuring `0`.
pub fn drive_points(
    config: &CollectorConfig,
    detected: usize,
    drives: &[DriveHealth],
    now: DateTime<Utc>,
) -> Vec<MetricPoint> {
    let mut points = vec![
        point(
            config,
            "node.disk_health_drives",
            drives.len() as f64,
            now,
            None,
        ),
        point(
            config,
            "node.disk_health_drives_detected",
            detected as f64,
            now,
            None,
        ),
    ];
    let wear: Vec<f64> = drives.iter().filter_map(|d| d.wear_percent).collect();
    if let Some(worst) = wear.iter().copied().reduce(f64::max) {
        points.push(point(config, "node.disk_wear_percent", worst, now, None));
    }
    let errors: Vec<u64> = drives.iter().filter_map(|d| d.media_errors).collect();
    if !errors.is_empty() {
        let sum = errors.iter().fold(0u64, |a, b| a.saturating_add(*b));
        points.push(point(
            config,
            "node.disk_media_errors",
            sum as f64,
            now,
            None,
        ));
    }
    let written: Vec<u64> = drives.iter().filter_map(|d| d.bytes_written).collect();
    if !written.is_empty() {
        let sum = written.iter().fold(0u64, |a, b| a.saturating_add(*b));
        points.push(point(
            config,
            "node.disk_bytes_written_total",
            sum as f64,
            now,
            None,
        ));
    }
    for d in drives {
        let label = Some(("device", d.device.as_str()));
        if let Some(w) = d.wear_percent {
            points.push(point(config, "node.disk_wear_percent", w, now, label));
        }
        if let Some(e) = d.media_errors {
            points.push(point(
                config,
                "node.disk_media_errors",
                e as f64,
                now,
                label,
            ));
        }
        if let Some(b) = d.bytes_written {
            points.push(point(
                config,
                "node.disk_bytes_written_total",
                b as f64,
                now,
                label,
            ));
        }
    }
    points
}

// ── Reports from worker nodes ─────────────────────────────────────────────────

/// Drive health as read on a host, for a worker node to send in its
/// heartbeat; the control plane turns it into the same `node.*` points the
/// local collector writes, under the worker's node id.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct DriveHealthReport {
    /// Physical drives found on the host.
    pub detected: u32,
    /// Drives whose SMART data could be read.
    pub drives: Vec<DriveHealth>,
}

/// One host's disk health. A section is `None` when the host could not
/// provide it (no `/proc/mdstat` because it is not Linux, or the first
/// SMART refresh has not finished), which is reported as "not reported"
/// rather than as healthy.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct DiskHealthReport {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raid_arrays: Option<Vec<RaidArray>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drives: Option<DriveHealthReport>,
}

/// Bounds applied to a report received over the network, so a worker can't
/// make the control plane write an unbounded number of series.
const MAX_REPORTED_ARRAYS: usize = 64;

impl DiskHealthReport {
    /// Read this host's disk health: `/proc/mdstat` now, drives from the
    /// cache (which refreshes itself in the background).
    pub async fn read(cache: &Arc<DriveHealthCache>) -> Self {
        let raid_arrays = tokio::fs::read_to_string("/proc/mdstat")
            .await
            .ok()
            .map(|mdstat| parse_mdstat(&mdstat));
        let drives = cache.current().map(|(detected, drives)| DriveHealthReport {
            detected: u32::try_from(detected).unwrap_or(u32::MAX),
            drives,
        });
        Self {
            raid_arrays,
            drives,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.raid_arrays.is_none() && self.drives.is_none()
    }

    /// The `node.*` points for this report, labelled with `config`'s source.
    /// Oversized lists from a remote host are truncated.
    pub fn points(&self, config: &CollectorConfig, now: DateTime<Utc>) -> Vec<MetricPoint> {
        let mut points = Vec::new();
        if let Some(arrays) = &self.raid_arrays {
            let arrays = &arrays[..arrays.len().min(MAX_REPORTED_ARRAYS)];
            points.extend(raid_points(config, arrays, now));
        }
        if let Some(report) = &self.drives {
            let drives = &report.drives[..report.drives.len().min(MAX_DRIVES)];
            points.extend(drive_points(config, report.detected as usize, drives, now));
        }
        points
    }
}

// ── Reading it back ───────────────────────────────────────────────────────────

/// One array in [`NodeDiskHealth`].
#[derive(Debug, Clone, PartialEq)]
pub struct RaidArrayStatus {
    pub name: String,
    pub degraded: bool,
    pub devices_failed: u32,
}

/// The latest disk health a node reported, per array and per drive.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct NodeDiskHealth {
    /// `false` when the node has never reported any disk-health metric
    /// (not Linux, or a collector from before disk health existed).
    pub reported: bool,
    pub raid_arrays: Vec<RaidArrayStatus>,
    pub drives: Vec<DriveHealth>,
    pub drives_detected: u32,
    pub drives_read: u32,
    /// When the node last reported any metric. The values above are the
    /// latest the store holds, so a node that stopped reporting keeps
    /// returning them; callers compare this with the current time to tell
    /// stale health from current health. `None` when it could not be read.
    pub reported_at: Option<DateTime<Utc>>,
}

const RAID_LABELLED: [&str; 2] = ["node.raid_degraded", "node.raid_devices_failed"];
const DRIVE_LABELLED: [&str; 3] = [
    "node.disk_wear_percent",
    "node.disk_media_errors",
    "node.disk_bytes_written_total",
];
const HEALTH_AGGREGATES: [&str; 3] = [
    "node.raid_arrays",
    "node.disk_health_drives",
    "node.disk_health_drives_detected",
];

/// Assemble [`NodeDiskHealth`] from the store's latest values.
pub fn assemble_node_disk_health(
    latest: &HashMap<String, f64>,
    arrays: &[crate::store::LabelledMetric],
    drives: &[crate::store::LabelledMetric],
) -> NodeDiskHealth {
    let count = |name: &str| latest.get(name).map(|v| v.max(0.0) as u32);
    let mut raid: std::collections::BTreeMap<&str, RaidArrayStatus> = Default::default();
    for m in arrays {
        let entry = raid
            .entry(m.label_value.as_str())
            .or_insert_with(|| RaidArrayStatus {
                name: m.label_value.clone(),
                degraded: false,
                devices_failed: 0,
            });
        match m.name.as_str() {
            "node.raid_degraded" => entry.degraded = m.value > 0.0,
            "node.raid_devices_failed" => entry.devices_failed = m.value.max(0.0) as u32,
            _ => {}
        }
    }
    let mut by_device: std::collections::BTreeMap<&str, DriveHealth> = Default::default();
    for m in drives {
        let entry = by_device
            .entry(m.label_value.as_str())
            .or_insert_with(|| DriveHealth {
                device: m.label_value.clone(),
                ..Default::default()
            });
        match m.name.as_str() {
            "node.disk_wear_percent" => entry.wear_percent = Some(m.value),
            "node.disk_media_errors" => entry.media_errors = Some(m.value.max(0.0) as u64),
            "node.disk_bytes_written_total" => entry.bytes_written = Some(m.value.max(0.0) as u64),
            _ => {}
        }
    }
    NodeDiskHealth {
        reported: HEALTH_AGGREGATES.iter().any(|n| latest.contains_key(*n)),
        raid_arrays: raid.into_values().collect(),
        drives: by_device.into_values().collect(),
        drives_detected: count("node.disk_health_drives_detected").unwrap_or(0),
        drives_read: count("node.disk_health_drives").unwrap_or(0),
        reported_at: None,
    }
}

/// Read a node's latest disk health from the metrics store.
pub async fn read_node_disk_health(
    store: &dyn crate::store::MetricsStore,
    node_id: i32,
) -> Result<NodeDiskHealth, crate::error::MetricsError> {
    use crate::store::{LatestByLabelQuery, LatestQuery};
    let latest = store
        .query_latest(LatestQuery {
            source_kind: SourceKind::Node,
            source_id: node_id,
            names: HEALTH_AGGREGATES.iter().map(|s| s.to_string()).collect(),
        })
        .await?;
    let arrays = store
        .query_latest_by_label(LatestByLabelQuery {
            source_kind: SourceKind::Node,
            source_id: node_id,
            names: RAID_LABELLED.iter().map(|s| s.to_string()).collect(),
            label_key: "array".into(),
        })
        .await?;
    let drives = store
        .query_latest_by_label(LatestByLabelQuery {
            source_kind: SourceKind::Node,
            source_id: node_id,
            names: DRIVE_LABELLED.iter().map(|s| s.to_string()).collect(),
            label_key: "device".into(),
        })
        .await?;
    let mut health = assemble_node_disk_health(&latest, &arrays, &drives);
    if health.reported {
        health.reported_at = store.latest_timestamp(SourceKind::Node, node_id).await?;
    }
    Ok(health)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Group labelled points by their label value, for tests and diagnostics.
    fn by_label<'a>(
        points: &'a [MetricPoint],
        key: &str,
    ) -> std::collections::BTreeMap<String, Vec<&'a MetricPoint>> {
        let mut out: std::collections::BTreeMap<String, Vec<&MetricPoint>> =
            std::collections::BTreeMap::new();
        for p in points {
            if let Some(v) = p.labels.get(key) {
                out.entry(v.clone()).or_default().push(p);
            }
        }
        out
    }

    fn config() -> CollectorConfig {
        CollectorConfig {
            source_id: 0,
            source_kind: SourceKind::Node,
            connection_string: "/".into(),
            environment: None,
            node_id: Some(0),
            timeout: Duration::from_secs(5),
        }
    }

    const MDSTAT_DEGRADED: &str = "\
Personalities : [raid1] [raid6] [raid5] [raid4]
md1 : active raid1 sdb2[1] sda2[0](F)
      976630464 blocks super 1.2 [2/1] [_U]
      bitmap: 3/8 pages [12KB], 65536KB chunk

md0 : active raid5 sdd1[3] sdc1[2] sdb1[1] sda1[0]
      2929893888 blocks super 1.2 level 5, 512k chunk, algorithm 2 [4/4] [UUUU]

md2 : active (auto-read-only) raid1 sdf1[1] sde1[0]
      1046528 blocks super 1.2 [2/2] [UU]
        resync=PENDING

md127 : inactive sdg[0](S)
      5288 blocks super external:imsm

md3 : active raid0 sdh1[1] sdi1[0]
      1953260544 blocks super 1.2 512k chunks

unused devices: <none>
";

    #[test]
    fn mdstat_finds_the_degraded_array_and_its_failed_member() {
        let arrays = parse_mdstat(MDSTAT_DEGRADED);
        let names: Vec<&str> = arrays.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, vec!["md1", "md0", "md2", "md127", "md3"]);

        let md1 = &arrays[0];
        assert_eq!(md1.level.as_deref(), Some("raid1"));
        assert_eq!(md1.members, Some((2, 1)));
        assert!(md1.degraded());
        assert_eq!(md1.devices_failed(), 1);

        assert!(!arrays[1].degraded(), "md0 [4/4] is healthy");
        assert_eq!(
            arrays[2].level.as_deref(),
            Some("raid1"),
            "(auto-read-only) skipped"
        );
        assert!(!arrays[2].degraded());
        assert!(
            !arrays[3].degraded(),
            "an inactive IMSM container is not degraded"
        );
        assert_eq!(arrays[3].level, None);
        assert_eq!(arrays[4].members, None, "raid0 has no redundancy status");
        assert!(!arrays[4].degraded());
    }

    #[test]
    fn mdstat_without_arrays_and_garbage_parse_to_nothing() {
        let empty = "Personalities : \nunused devices: <none>\n";
        assert!(parse_mdstat(empty).is_empty());
        assert!(parse_mdstat("").is_empty());
        assert!(parse_mdstat("not mdstat at all : x\n[2/1]").is_empty());
    }

    #[test]
    fn raid_points_have_an_aggregate_and_one_series_per_array() {
        let arrays = parse_mdstat(MDSTAT_DEGRADED);
        let points = raid_points(&config(), &arrays, Utc::now());
        let aggregate = |name: &str| {
            points
                .iter()
                .find(|p| p.name == name && p.labels.is_empty())
                .map(|p| p.value)
        };
        assert_eq!(aggregate("node.raid_arrays"), Some(5.0));
        assert_eq!(aggregate("node.raid_degraded"), Some(1.0));
        assert_eq!(aggregate("node.raid_devices_failed"), Some(1.0));
        let per_array = by_label(&points, "array");
        let md1 = &per_array["md1"];
        assert!(md1
            .iter()
            .any(|p| p.name == "node.raid_degraded" && p.value == 1.0));
        assert!(per_array["md0"].iter().all(|p| p.value == 0.0));
    }

    #[test]
    fn no_raid_still_reports_zero_arrays() {
        let points = raid_points(&config(), &[], Utc::now());
        assert_eq!(points.len(), 3);
        assert!(points.iter().all(|p| p.value == 0.0 && p.labels.is_empty()));
    }

    fn nvme_log(percentage_used: u8, units_written: u128, media_errors: u128) -> [u8; 512] {
        let mut log = [0u8; 512];
        log[5] = percentage_used;
        log[48..64].copy_from_slice(&units_written.to_le_bytes());
        log[160..176].copy_from_slice(&media_errors.to_le_bytes());
        log
    }

    #[test]
    fn nvme_smart_log_fields_are_decoded() {
        let h = parse_nvme_smart_log("nvme0", &nvme_log(83, 2_000_000, 4));
        assert_eq!(h.device, "nvme0");
        assert_eq!(h.wear_percent, Some(83.0));
        assert_eq!(h.media_errors, Some(4));
        // 2,000,000 data units × 512,000 bytes = 1.024 TB.
        assert_eq!(h.bytes_written, Some(1_024_000_000_000));
    }

    #[test]
    fn nvme_counters_saturate_instead_of_wrapping() {
        let h = parse_nvme_smart_log("nvme1", &nvme_log(255, u128::MAX, u128::MAX));
        assert_eq!(h.wear_percent, Some(255.0));
        assert_eq!(h.media_errors, Some(u64::MAX));
        assert_eq!(h.bytes_written, Some(u64::MAX));
    }

    #[test]
    fn smartctl_nvme_json_is_decoded() {
        let json = serde_json::json!({
            "nvme_smart_health_information_log": {
                "percentage_used": 12,
                "media_errors": 0,
                "data_units_written": 1000
            }
        });
        let h = parse_smartctl_json("nvme0", &json).unwrap();
        assert_eq!(h.wear_percent, Some(12.0));
        assert_eq!(h.media_errors, Some(0));
        assert_eq!(h.bytes_written, Some(512_000_000));
    }

    fn ata_row(id: u64, value: u64, raw: u64) -> serde_json::Value {
        serde_json::json!({ "id": id, "value": value, "raw": { "value": raw } })
    }

    #[test]
    fn smartctl_ata_json_uses_life_left_sectors_and_lbas() {
        let json = serde_json::json!({
            "logical_block_size": 512,
            "ata_smart_attributes": { "table": [
                ata_row(5, 100, 8),
                ata_row(177, 91, 120),
                ata_row(197, 100, 2),
                ata_row(241, 99, 1_000_000),
            ]}
        });
        let h = parse_smartctl_json("sda", &json).unwrap();
        assert_eq!(
            h.wear_percent,
            Some(9.0),
            "Wear_Leveling_Count 91 → 9% used"
        );
        assert_eq!(h.media_errors, Some(10), "reallocated + pending");
        assert_eq!(h.bytes_written, Some(512_000_000));
    }

    #[test]
    fn smartctl_endurance_used_wins_and_hdds_have_no_wear() {
        let json = serde_json::json!({
            "endurance_used": { "current_percent": 40 },
            "ata_smart_attributes": { "table": [ ata_row(177, 91, 0) ] }
        });
        assert_eq!(
            parse_smartctl_json("sda", &json).unwrap().wear_percent,
            Some(40.0)
        );
        let hdd = serde_json::json!({
            "ata_smart_attributes": { "table": [ ata_row(5, 100, 0) ] }
        });
        let h = parse_smartctl_json("sdb", &hdd).unwrap();
        assert_eq!(h.wear_percent, None);
        assert_eq!(h.media_errors, Some(0));
        assert_eq!(h.bytes_written, None);
    }

    #[test]
    fn smartctl_without_health_data_is_none() {
        let json = serde_json::json!({ "smartctl": { "exit_status": 2 } });
        assert!(parse_smartctl_json("sda", &json).is_none());
    }

    /// A SAS SSD as `smartctl -j -A -i -l error -l ssd` reports it: no ATA
    /// attribute table, endurance from the SSD page, uncorrected errors and
    /// gigabytes (a decimal string) from the error counter log.
    #[test]
    fn smartctl_scsi_json_reads_endurance_error_counters_and_writes() {
        let json = serde_json::json!({
            "device": { "name": "/dev/sdc", "type": "scsi", "protocol": "SCSI" },
            "scsi_grown_defect_list": 2,
            "scsi_percentage_used_endurance_indicator": 7,
            "scsi_error_counter_log": {
                "read": {
                    "errors_corrected_by_eccfast": 0,
                    "total_errors_corrected": 12,
                    "gigabytes_processed": "48211.990",
                    "total_uncorrected_errors": 1
                },
                "write": {
                    "total_errors_corrected": 0,
                    "gigabytes_processed": "1500.250",
                    "total_uncorrected_errors": 0
                },
                "verify": {
                    "total_errors_corrected": 0,
                    "gigabytes_processed": "0.000",
                    "total_uncorrected_errors": 3
                }
            }
        });
        let h = parse_smartctl_json("sdc", &json).expect("SCSI health");
        assert_eq!(h.device, "sdc");
        assert_eq!(h.wear_percent, Some(7.0));
        assert_eq!(h.media_errors, Some(2 + 1 + 3));
        assert_eq!(h.bytes_written, Some(1_500_250_000_000));
    }

    /// A SAS hard disk has no endurance figure; the error counters still
    /// count, and newer smartctl's `endurance_used` is honoured for SSDs.
    #[test]
    fn smartctl_scsi_hdd_has_no_wear_and_endurance_used_is_read() {
        let hdd = serde_json::json!({
            "device": { "type": "scsi" },
            "scsi_error_counter_log": {
                "read": { "total_uncorrected_errors": 0 },
                "write": { "total_uncorrected_errors": 0 }
            }
        });
        let h = parse_smartctl_json("sdd", &hdd).expect("SCSI health");
        assert_eq!(h.wear_percent, None);
        assert_eq!(h.media_errors, Some(0));
        assert_eq!(h.bytes_written, None);

        let ssd = serde_json::json!({ "endurance_used": { "current_percent": 12 } });
        assert_eq!(
            parse_smartctl_json("sde", &ssd)
                .expect("health")
                .wear_percent,
            Some(12.0)
        );
    }

    #[test]
    fn drive_points_aggregate_worst_wear_and_summed_errors() {
        let drives = vec![
            DriveHealth {
                device: "nvme0".into(),
                wear_percent: Some(85.0),
                media_errors: Some(0),
                bytes_written: Some(100),
            },
            DriveHealth {
                device: "sda".into(),
                wear_percent: None,
                media_errors: Some(3),
                bytes_written: None,
            },
        ];
        let points = drive_points(&config(), 3, &drives, Utc::now());
        let aggregate = |name: &str| {
            points
                .iter()
                .find(|p| p.name == name && p.labels.is_empty())
                .map(|p| p.value)
        };
        assert_eq!(aggregate("node.disk_wear_percent"), Some(85.0));
        assert_eq!(aggregate("node.disk_media_errors"), Some(3.0));
        assert_eq!(aggregate("node.disk_bytes_written_total"), Some(100.0));
        assert_eq!(aggregate("node.disk_health_drives"), Some(2.0));
        assert_eq!(aggregate("node.disk_health_drives_detected"), Some(3.0));
        let per_device = by_label(&points, "device");
        assert_eq!(per_device["sda"].len(), 1, "only the field sda reports");
    }

    #[test]
    fn no_readable_drive_emits_no_wear_or_error_values() {
        let points = drive_points(&config(), 2, &[], Utc::now());
        let names: Vec<&str> = points.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "node.disk_health_drives",
                "node.disk_health_drives_detected"
            ]
        );
    }

    #[test]
    fn drives_are_listed_from_sys_block() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for name in [
            "nvme0n1",
            "nvme0n2",
            "nvme1n1",
            "nvme1c1n1",
            "sda",
            "sdb",
            "sdc",
            "vda",
            "loop0",
            "dm-0",
            "md0",
            "zram0",
        ] {
            std::fs::create_dir(root.join(name)).unwrap();
        }
        std::fs::write(root.join("sda/removable"), "0\n").unwrap();
        std::fs::write(root.join("sdc/removable"), "1\n").unwrap();
        let drives = list_drives(root);
        let names: Vec<(&str, DriveKind)> =
            drives.iter().map(|d| (d.name.as_str(), d.kind)).collect();
        assert_eq!(
            names,
            vec![
                ("nvme0", DriveKind::Nvme),
                ("nvme1", DriveKind::Nvme),
                ("sda", DriveKind::Scsi),
                ("sdb", DriveKind::Scsi),
            ]
        );
        assert!(list_drives(&root.join("missing")).is_empty());
    }

    #[test]
    fn a_report_round_trips_and_yields_the_same_points_as_local_collection() {
        let config = config();
        let now = Utc::now();
        let arrays =
            parse_mdstat("md0 : active raid1 sdb1[1] sda1[0]\n      1000 blocks [2/1] [U_]\n");
        let drives = vec![DriveHealth {
            device: "nvme0".into(),
            wear_percent: Some(12.0),
            media_errors: Some(0),
            bytes_written: Some(1_000),
        }];
        let report = DiskHealthReport {
            raid_arrays: Some(arrays.clone()),
            drives: Some(DriveHealthReport {
                detected: 1,
                drives: drives.clone(),
            }),
        };
        let wire = serde_json::to_string(&report).expect("serialize");
        let back: DiskHealthReport = serde_json::from_str(&wire).expect("deserialize");
        assert_eq!(back, report);

        // (metric name, value bits, sorted labels): order-independent identity.
        type PointKey = (String, u64, Vec<(String, String)>);
        let key = |points: Vec<MetricPoint>| -> Vec<PointKey> {
            points
                .into_iter()
                .map(|p| {
                    let mut labels: Vec<_> = p.labels.into_iter().collect();
                    labels.sort();
                    (p.name, p.value.to_bits(), labels)
                })
                .collect()
        };
        let mut local = raid_points(&config, &arrays, now);
        local.extend(drive_points(&config, 1, &drives, now));
        assert_eq!(key(back.points(&config, now)), key(local));
    }

    #[test]
    fn an_empty_or_old_report_yields_no_points() {
        // A heartbeat from an agent that predates disk health has no field.
        let old: DiskHealthReport = serde_json::from_str("{}").expect("deserialize");
        assert!(old.is_empty());
        assert!(old.points(&config(), Utc::now()).is_empty());
    }

    #[test]
    fn an_oversized_remote_report_is_truncated() {
        let report = DiskHealthReport {
            raid_arrays: None,
            drives: Some(DriveHealthReport {
                detected: 10_000,
                drives: (0..10_000)
                    .map(|i| DriveHealth {
                        device: format!("sd{i}"),
                        wear_percent: Some(1.0),
                        ..Default::default()
                    })
                    .collect(),
            }),
        };
        let points = report.points(&config(), Utc::now());
        let per_device = points
            .iter()
            .filter(|p| p.labels.contains_key("device"))
            .count();
        assert_eq!(per_device, MAX_DRIVES);
    }

    fn labelled(label: &str, name: &str, value: f64) -> crate::store::LabelledMetric {
        crate::store::LabelledMetric {
            label_value: label.into(),
            name: name.into(),
            value,
        }
    }

    #[test]
    fn stored_values_assemble_into_per_array_and_per_drive_rows() {
        let latest = HashMap::from([
            ("node.raid_arrays".to_string(), 2.0),
            ("node.disk_health_drives".to_string(), 1.0),
            ("node.disk_health_drives_detected".to_string(), 2.0),
        ]);
        let arrays = vec![
            labelled("md1", "node.raid_degraded", 1.0),
            labelled("md1", "node.raid_devices_failed", 1.0),
            labelled("md0", "node.raid_degraded", 0.0),
            labelled("md0", "node.raid_devices_failed", 0.0),
        ];
        let drives = vec![
            labelled("nvme0", "node.disk_wear_percent", 85.0),
            labelled("nvme0", "node.disk_media_errors", 0.0),
            labelled("nvme0", "node.disk_bytes_written_total", 1e12),
        ];
        let health = assemble_node_disk_health(&latest, &arrays, &drives);
        assert!(health.reported);
        assert_eq!(health.raid_arrays.len(), 2);
        assert_eq!(health.raid_arrays[0].name, "md0", "sorted by name");
        assert!(health.raid_arrays[1].degraded);
        assert_eq!(health.raid_arrays[1].devices_failed, 1);
        assert_eq!(health.drives[0].wear_percent, Some(85.0));
        assert_eq!(health.drives[0].bytes_written, Some(1_000_000_000_000));
        assert_eq!((health.drives_read, health.drives_detected), (1, 2));
    }

    #[test]
    fn nothing_stored_is_not_reported() {
        let health = assemble_node_disk_health(&HashMap::new(), &[], &[]);
        assert!(!health.reported);
        assert!(health.raid_arrays.is_empty() && health.drives.is_empty());
    }

    #[tokio::test]
    async fn the_cache_reports_nothing_until_a_refresh_completes() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DriveHealthCache::new(dir.path());
        assert!(cache.current().is_none(), "no reading yet");
        cache.refresh().await;
        assert_eq!(
            cache.current(),
            Some((0, Vec::new())),
            "no drives under the dir"
        );
    }

    #[test]
    fn nvme_and_scsi_names_are_recognised_exactly() {
        assert_eq!(nvme_controller_of("nvme12n1").as_deref(), Some("nvme12"));
        assert_eq!(nvme_controller_of("nvme0c3n1").as_deref(), Some("nvme0"));
        assert_eq!(nvme_controller_of("nvme0"), None, "the controller itself");
        assert_eq!(nvme_controller_of("nvme-fabrics"), None);
        assert!(is_scsi_disk("sdab"));
        assert!(!is_scsi_disk("sda1"));
        assert!(!is_scsi_disk("sd"));
    }
}
