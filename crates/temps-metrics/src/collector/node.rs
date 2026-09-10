// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Node-level system metrics collector (Linux `/proc` + `statvfs(2)`).
//!
//! Unlike the database collectors, this collector reads local kernel interfaces
//! rather than connecting to a remote service.  The `connection_string` field
//! of [`CollectorConfig`] is repurposed to carry the `data_dir` path whose
//! filesystem is monitored for disk usage.
//!
//! ## CPU delta handling
//!
//! CPU utilisation must be computed as a delta between two `/proc/stat`
//! readings.  Because the [`Collector`] trait is otherwise stateless, the node
//! collector stores the previous CPU tick snapshot inside a `Mutex`.  The first
//! call after construction establishes the baseline and emits no `cpu_percent`
//! metric; subsequent calls emit the delta.
//!
//! ## Metrics emitted
//!
//! | Name | Kind | Description |
//! |---|---|---|
//! | `node.cpu_percent` | Gauge | CPU utilisation since last scrape (0–100) |
//! | `node.memory_used_bytes` | Gauge | MemTotal − MemAvailable |
//! | `node.memory_total_bytes` | Gauge | MemTotal |
//! | `node.memory_percent` | Gauge | used / total × 100 |
//! | `node.load_avg_1m` | Gauge | 1-minute load average |
//! | `node.load_avg_5m` | Gauge | 5-minute load average |
//! | `node.load_avg_15m` | Gauge | 15-minute load average |
//! | `node.disk_used_bytes` | Gauge | Disk space used under `data_dir` |
//! | `node.disk_total_bytes` | Gauge | Total disk space under `data_dir` |
//! | `node.disk_percent` | Gauge | used / total × 100 |
//! | `node.fd_allocated` | Gauge | System-wide allocated file handles (`/proc/sys/fs/file-nr`) |
//! | `node.fd_max` | Gauge | System-wide file handle ceiling (`fs.file-max`) |
//! | `node.fd_percent` | Gauge | allocated / max × 100 — sockets are file descriptors, so this is the machine-wide "running out of sockets" signal |
//! | `node.process_open_fds` | Gauge | This process's own open file descriptor count (`/proc/self/fd`) |
//! | `node.process_fd_limit` | Gauge | This process's soft `RLIMIT_NOFILE` |
//! | `node.process_fd_percent` | Gauge | open / limit × 100 — this process hitting its own ceiling, independent of the system-wide one |
//! | `node.disk_read_bytes_total` | Gauge (cumulative) | Bytes read from physical block devices since boot (`/proc/diskstats`) |
//! | `node.disk_write_bytes_total` | Gauge (cumulative) | Bytes written to physical block devices since boot (`/proc/diskstats`) |
//! | `node.network_rx_bytes_total` | Gauge (cumulative) | Bytes received on non-virtual interfaces since boot (`/proc/net/dev`) |
//! | `node.network_tx_bytes_total` | Gauge (cumulative) | Bytes transmitted on non-virtual interfaces since boot (`/proc/net/dev`) |
//!
//! ## Cumulative I/O counters
//!
//! The four `*_bytes_total` series are stored as the raw cumulative kernel
//! counter, exactly like OTLP counters on the ingest path. They are emitted
//! as `MetricKind::Gauge` because this collector bypasses the scraper's
//! in-memory delta computation (`NodeMetricsSampler` writes straight to the
//! store). The `_total` suffix makes [`crate::is_monotonic_counter`] true on
//! the read path, so `query_range` returns the per-bucket *increase* (LAG
//! over the bucketed max, floored at 0 across reboots) — callers divide by
//! the bucket width to get a throughput.
//!
//! ## Non-Linux behaviour
//!
//! When `/proc` is absent (macOS, FreeBSD, Windows) every `/proc` read
//! degrades gracefully: the collector returns whatever metrics it could collect
//! and silently skips the rest.  No error is propagated.
//!
//! On non-Linux targets CPU, memory, block I/O and network I/O fall back to
//! the cross-platform `sysinfo` crate so a developer box still renders a
//! populated server-monitoring page. The file-descriptor and load-average
//! series stay Linux-only.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use tracing::{debug, warn};

use super::{Collector, CollectorConfig};
use crate::error::MetricsError;
use crate::store::{MetricKind, MetricPoint, SourceKind};

/// Snapshot of CPU tick counters from `/proc/stat` for the aggregate `cpu` line.
#[derive(Debug, Clone, Default)]
struct CpuSnapshot {
    idle: u64,
    total: u64,
}

/// Node metric collector.
///
/// Holds CPU baseline state across calls inside a `Mutex` so the collector can
/// implement `Sync` while mutating the snapshot.  All other metrics are
/// stateless reads from `/proc` or `statvfs(2)`.
pub struct NodeMetricsCollector {
    /// Previous CPU tick snapshot; `None` before the first scrape.
    prev_cpu: Mutex<Option<CpuSnapshot>>,
    /// Cross-platform fallback state (non-Linux only). `sysinfo` computes CPU
    /// usage as the delta since the previous refresh, so the `System` handle
    /// must outlive a single scrape — same reason `prev_cpu` exists above.
    #[cfg(not(target_os = "linux"))]
    fallback: Mutex<fallback::FallbackState>,
}

impl NodeMetricsCollector {
    pub fn new() -> Self {
        Self {
            prev_cpu: Mutex::new(None),
            #[cfg(not(target_os = "linux"))]
            fallback: Mutex::new(fallback::FallbackState::new()),
        }
    }
}

impl Default for NodeMetricsCollector {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Collector for NodeMetricsCollector {
    fn engine(&self) -> &'static str {
        "node"
    }

    async fn collect(&self, config: &CollectorConfig) -> Result<Vec<MetricPoint>, MetricsError> {
        let source_id = config.source_id;
        let data_dir = config.connection_string.as_str();

        debug!(
            source_id,
            engine = "node",
            "starting node metric collection"
        );

        let now = Utc::now();
        let mut points = Vec::new();

        // CPU — synchronous (no I/O latency).
        points.extend(self.collect_cpu(source_id, config, now));

        // Memory — async file read.
        points.extend(collect_memory(source_id, config, now).await);

        // Load average — async file read.
        points.extend(collect_loadavg(source_id, config, now).await);

        // Disk — synchronous statvfs call.
        points.extend(collect_disk(source_id, config, Path::new(data_dir), now));

        // File descriptors — synchronous /proc reads + getrlimit(2).
        points.extend(collect_system_fds(source_id, config, now));
        points.extend(collect_process_fds(source_id, config, now));

        // Block + network I/O — cumulative kernel counters.
        points.extend(collect_disk_io(source_id, config, now).await);
        points.extend(collect_network_io(source_id, config, now).await);

        // Non-Linux: fill whatever `/proc` could not answer from `sysinfo`.
        #[cfg(not(target_os = "linux"))]
        points.extend(self.collect_fallback(source_id, config, now));

        debug!(
            source_id,
            engine = "node",
            metric_count = points.len(),
            "finished node metric collection"
        );

        Ok(points)
    }
}

// ── CPU (/proc/stat) ──────────────────────────────────────────────────────────

impl NodeMetricsCollector {
    fn collect_cpu(
        &self,
        source_id: i32,
        config: &CollectorConfig,
        now: DateTime<Utc>,
    ) -> Vec<MetricPoint> {
        let snapshot = match read_cpu_snapshot() {
            Some(s) => s,
            None => return Vec::new(),
        };

        let mut guard = match self.prev_cpu.lock() {
            Ok(g) => g,
            Err(e) => {
                warn!(source_id, "CPU snapshot mutex poisoned: {e}");
                return Vec::new();
            }
        };

        let cpu_percent = match guard.as_ref() {
            None => {
                // First scrape — establish baseline; emit nothing.
                *guard = Some(snapshot);
                return Vec::new();
            }
            Some(prev) => {
                let delta_idle = snapshot.idle.saturating_sub(prev.idle) as f64;
                let delta_total = snapshot.total.saturating_sub(prev.total) as f64;
                let pct = if delta_total > 0.0 {
                    (1.0 - delta_idle / delta_total) * 100.0
                } else {
                    0.0
                };
                *guard = Some(snapshot);
                pct.clamp(0.0, 100.0)
            }
        };

        vec![gauge(
            source_id,
            config,
            "node.cpu_percent",
            cpu_percent,
            now,
        )]
    }
}

/// Read the `cpu ` aggregate line from `/proc/stat`.
///
/// Returns `None` when `/proc/stat` is absent or unparsable (non-Linux).
fn read_cpu_snapshot() -> Option<CpuSnapshot> {
    let content = std::fs::read_to_string("/proc/stat").ok()?;
    // The first line: `cpu  <user> <nice> <system> <idle> <iowait> <irq> <softirq> ...`
    let line = content.lines().find(|l| l.starts_with("cpu "))?;
    let fields: Vec<u64> = line
        .split_whitespace()
        .skip(1) // skip "cpu"
        .map(|v| v.parse().ok())
        .collect::<Option<Vec<_>>>()?;

    // Kernel field layout (see `man 5 proc`):
    //   0=user 1=nice 2=system 3=idle 4=iowait 5=irq 6=softirq ...
    // iowait (index 4) counts as idle time for our utilisation formula.
    let idle = fields.get(3).copied().unwrap_or(0) + fields.get(4).copied().unwrap_or(0);
    let total: u64 = fields.iter().sum();

    Some(CpuSnapshot { idle, total })
}

// ── Memory (/proc/meminfo) ────────────────────────────────────────────────────

/// Parse `Field:   value kB` lines from `/proc/meminfo`.
fn parse_meminfo_field(content: &str, field: &str) -> Option<u64> {
    content.lines().find_map(|line| {
        let line = line.trim();
        let rest = line.strip_prefix(field)?.trim();
        let rest = rest.strip_prefix(':')?.trim();
        // Value may be suffixed with " kB" — we only need the number.
        rest.split_whitespace().next().and_then(|v| v.parse().ok())
    })
}

async fn collect_memory(
    source_id: i32,
    config: &CollectorConfig,
    now: DateTime<Utc>,
) -> Vec<MetricPoint> {
    let content = match tokio::fs::read_to_string("/proc/meminfo").await {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    let total_kb = match parse_meminfo_field(&content, "MemTotal") {
        Some(v) => v,
        None => {
            warn!(source_id, "Could not parse MemTotal from /proc/meminfo");
            return Vec::new();
        }
    };

    let available_kb = match parse_meminfo_field(&content, "MemAvailable") {
        Some(v) => v,
        None => {
            warn!(source_id, "Could not parse MemAvailable from /proc/meminfo");
            return Vec::new();
        }
    };

    let total_bytes = total_kb * 1024;
    let used_bytes = total_kb.saturating_sub(available_kb) * 1024;
    let percent = if total_bytes > 0 {
        (used_bytes as f64 / total_bytes as f64) * 100.0
    } else {
        0.0
    };

    vec![
        gauge(
            source_id,
            config,
            "node.memory_used_bytes",
            used_bytes as f64,
            now,
        ),
        gauge(
            source_id,
            config,
            "node.memory_total_bytes",
            total_bytes as f64,
            now,
        ),
        gauge(source_id, config, "node.memory_percent", percent, now),
    ]
}

// ── Load average (/proc/loadavg) ──────────────────────────────────────────────

async fn collect_loadavg(
    source_id: i32,
    config: &CollectorConfig,
    now: DateTime<Utc>,
) -> Vec<MetricPoint> {
    let content = match tokio::fs::read_to_string("/proc/loadavg").await {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    // Format: `0.12 0.34 0.56 1/234 5678`
    let mut fields = content.split_whitespace();
    let avg1: f64 = fields.next().and_then(|v| v.parse().ok()).unwrap_or(0.0);
    let avg5: f64 = fields.next().and_then(|v| v.parse().ok()).unwrap_or(0.0);
    let avg15: f64 = fields.next().and_then(|v| v.parse().ok()).unwrap_or(0.0);

    vec![
        gauge(source_id, config, "node.load_avg_1m", avg1, now),
        gauge(source_id, config, "node.load_avg_5m", avg5, now),
        gauge(source_id, config, "node.load_avg_15m", avg15, now),
    ]
}

// ── Disk (statvfs) ────────────────────────────────────────────────────────────

fn collect_disk(
    source_id: i32,
    config: &CollectorConfig,
    data_dir: &Path,
    now: DateTime<Utc>,
) -> Vec<MetricPoint> {
    #[cfg(unix)]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let path_cstr = match CString::new(data_dir.as_os_str().as_bytes()) {
            Ok(c) => c,
            Err(e) => {
                warn!(
                    source_id,
                    "data_dir contains null byte, cannot statvfs: {e}"
                );
                return Vec::new();
            }
        };

        // SAFETY: `path_cstr` is a valid NUL-terminated C string and
        // `buf` is a zeroed, properly sized stack allocation.
        let mut buf: libc::statvfs = unsafe { std::mem::zeroed() };
        let rc = unsafe { libc::statvfs(path_cstr.as_ptr(), &mut buf) };

        if rc != 0 {
            let err = std::io::Error::last_os_error();
            warn!(
                source_id,
                path = %data_dir.display(),
                "statvfs failed: {err}"
            );
            return Vec::new();
        }

        // POSIX: `f_blocks` / `f_bavail` are counted in units of `f_frsize`
        // (the fragment size), not `f_bsize` (the preferred I/O size). They
        // are equal on Linux ext4/xfs, but on macOS APFS `f_bsize` is 1 MiB
        // while `f_frsize` is 4 KiB — using `f_bsize` there reports a 4 TB
        // disk as ~860 TB. Fall back to `f_bsize` only if `f_frsize` is 0.
        let bsize = if buf.f_frsize > 0 {
            buf.f_frsize as u64
        } else {
            buf.f_bsize as u64
        };
        let total_bytes = buf.f_blocks as u64 * bsize;
        let avail_bytes = buf.f_bavail as u64 * bsize;
        let used_bytes = total_bytes.saturating_sub(avail_bytes);
        let percent = if total_bytes > 0 {
            (used_bytes as f64 / total_bytes as f64) * 100.0
        } else {
            0.0
        };

        vec![
            gauge(
                source_id,
                config,
                "node.disk_used_bytes",
                used_bytes as f64,
                now,
            ),
            gauge(
                source_id,
                config,
                "node.disk_total_bytes",
                total_bytes as f64,
                now,
            ),
            gauge(source_id, config, "node.disk_percent", percent, now),
        ]
    }

    // Non-Unix platforms: graceful degradation.
    #[cfg(not(unix))]
    {
        let _ = (source_id, config, data_dir, now);
        Vec::new()
    }
}

// ── File descriptors, system-wide (/proc/sys/fs/file-nr) ──────────────────────

/// Read the machine-wide count of allocated file handles and the kernel
/// ceiling on that count. Every open socket, pipe and file counts against
/// this the same way, so it is the definitive "is this machine about to
/// start refusing new connections with `ENFILE`" signal — independent of any
/// single process's own limit.
fn collect_system_fds(
    source_id: i32,
    config: &CollectorConfig,
    now: DateTime<Utc>,
) -> Vec<MetricPoint> {
    let content = match std::fs::read_to_string("/proc/sys/fs/file-nr") {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    // Format: "<allocated>\t<free (always 0 on Linux >= 2.6, unused)>\t<max>"
    let mut fields = content.split_whitespace();
    let allocated: f64 = match fields.next().and_then(|v| v.parse().ok()) {
        Some(v) => v,
        None => {
            warn!(
                source_id,
                "could not parse allocated fd count from /proc/sys/fs/file-nr"
            );
            return Vec::new();
        }
    };
    let _unused = fields.next();
    let max: f64 = match fields.next().and_then(|v| v.parse().ok()) {
        Some(v) => v,
        None => {
            warn!(
                source_id,
                "could not parse fd-max from /proc/sys/fs/file-nr"
            );
            return Vec::new();
        }
    };

    let percent = if max > 0.0 {
        (allocated / max) * 100.0
    } else {
        0.0
    };

    vec![
        gauge(source_id, config, "node.fd_allocated", allocated, now),
        gauge(source_id, config, "node.fd_max", max, now),
        gauge(source_id, config, "node.fd_percent", percent, now),
    ]
}

// ── File descriptors, this process (/proc/self/fd + getrlimit) ────────────────

/// Read this process's own open file descriptor count against its soft
/// `RLIMIT_NOFILE`. This is the process most likely to be holding the
/// sockets on a temps box (the proxy), so it can hit its own ceiling well
/// before the system-wide one is anywhere close.
fn collect_process_fds(
    source_id: i32,
    config: &CollectorConfig,
    now: DateTime<Utc>,
) -> Vec<MetricPoint> {
    #[cfg(unix)]
    {
        // Counting directory entries under /proc/self/fd is the standard way
        // to read a process's own open-fd count on Linux. This includes the
        // directory handle opened to perform the listing itself (a constant
        // +1), which is negligible against real usage. Not present on
        // non-Linux unix (e.g. macOS has no /proc): degrades gracefully.
        let open_fds = match std::fs::read_dir("/proc/self/fd") {
            Ok(entries) => entries.count() as f64,
            Err(_) => return Vec::new(),
        };

        let limit = match read_nofile_soft_limit() {
            Some(v) => v,
            None => {
                // RLIM_INFINITY or a getrlimit(2) failure: still report the
                // raw count, just skip the percent metric with no ceiling to
                // divide by.
                return vec![gauge(
                    source_id,
                    config,
                    "node.process_open_fds",
                    open_fds,
                    now,
                )];
            }
        };

        let percent = if limit > 0.0 {
            (open_fds / limit) * 100.0
        } else {
            0.0
        };

        vec![
            gauge(source_id, config, "node.process_open_fds", open_fds, now),
            gauge(source_id, config, "node.process_fd_limit", limit, now),
            gauge(source_id, config, "node.process_fd_percent", percent, now),
        ]
    }

    #[cfg(not(unix))]
    {
        let _ = (source_id, config, now);
        Vec::new()
    }
}

#[cfg(unix)]
fn read_nofile_soft_limit() -> Option<f64> {
    // SAFETY: RLIMIT_NOFILE with a zeroed, properly sized stack allocation is
    // the standard getrlimit(2) call.
    let mut limit: libc::rlimit = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) };
    if rc != 0 {
        return None;
    }
    if limit.rlim_cur == libc::RLIM_INFINITY {
        return None;
    }
    Some(limit.rlim_cur as f64)
}

// ── Block I/O (/proc/diskstats) ───────────────────────────────────────────────

/// Sector size used by `/proc/diskstats`. The kernel always reports sectors
/// in 512-byte units here regardless of the device's physical sector size.
const DISKSTATS_SECTOR_BYTES: u64 = 512;

/// Cumulative bytes read / written across physical block devices.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct DiskIoTotals {
    read_bytes: u64,
    write_bytes: u64,
}

/// Whether a `/proc/diskstats` device name is a virtual / layered device whose
/// traffic is already counted on the physical device underneath it (or is not
/// a disk at all). Summing these would double-count.
fn is_virtual_block_device(name: &str) -> bool {
    const PREFIXES: [&str; 8] = ["loop", "ram", "zram", "dm-", "md", "sr", "fd", "nbd"];
    PREFIXES.iter().any(|p| name.starts_with(p))
}

/// Parse `/proc/diskstats` and sum sectors read/written over whole physical
/// devices only.
///
/// Partitions are excluded because their I/O is already included in the
/// parent device's counters: a device is treated as a partition when another
/// listed device name is a strict prefix of it (`sda` → `sda1`,
/// `nvme0n1` → `nvme0n1p1`, `mmcblk0` → `mmcblk0p2`).
fn parse_diskstats(content: &str) -> Option<DiskIoTotals> {
    // (name, sectors_read, sectors_written)
    let mut devices: Vec<(&str, u64, u64)> = Vec::new();
    for line in content.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        // Layout (man 5 proc, "diskstats"): major minor name reads_completed
        // reads_merged sectors_read ms_reading writes_completed writes_merged
        // sectors_written ...
        if fields.len() < 10 {
            continue;
        }
        let name = fields[2];
        if is_virtual_block_device(name) {
            continue;
        }
        let sectors_read: u64 = fields[5].parse().ok()?;
        let sectors_written: u64 = fields[9].parse().ok()?;
        devices.push((name, sectors_read, sectors_written));
    }
    if devices.is_empty() {
        return None;
    }

    let mut totals = DiskIoTotals::default();
    for (name, sectors_read, sectors_written) in &devices {
        let is_partition = devices
            .iter()
            .any(|(other, _, _)| other.len() < name.len() && name.starts_with(other));
        if is_partition {
            continue;
        }
        totals.read_bytes = totals
            .read_bytes
            .saturating_add(sectors_read.saturating_mul(DISKSTATS_SECTOR_BYTES));
        totals.write_bytes = totals
            .write_bytes
            .saturating_add(sectors_written.saturating_mul(DISKSTATS_SECTOR_BYTES));
    }
    Some(totals)
}

async fn collect_disk_io(
    source_id: i32,
    config: &CollectorConfig,
    now: DateTime<Utc>,
) -> Vec<MetricPoint> {
    let content = match tokio::fs::read_to_string("/proc/diskstats").await {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let totals = match parse_diskstats(&content) {
        Some(t) => t,
        None => {
            debug!(
                source_id,
                "no physical block devices found in /proc/diskstats"
            );
            return Vec::new();
        }
    };
    disk_io_points(source_id, config, totals, now)
}

fn disk_io_points(
    source_id: i32,
    config: &CollectorConfig,
    totals: DiskIoTotals,
    now: DateTime<Utc>,
) -> Vec<MetricPoint> {
    vec![
        gauge(
            source_id,
            config,
            "node.disk_read_bytes_total",
            totals.read_bytes as f64,
            now,
        ),
        gauge(
            source_id,
            config,
            "node.disk_write_bytes_total",
            totals.write_bytes as f64,
            now,
        ),
    ]
}

// ── Network I/O (/proc/net/dev) ───────────────────────────────────────────────

/// Cumulative bytes received / transmitted across non-virtual interfaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct NetworkIoTotals {
    rx_bytes: u64,
    tx_bytes: u64,
}

/// Whether an interface is the loopback or a container-side virtual link.
/// Traffic on `veth*` / `docker*` / `br-*` is either purely local
/// (container ↔ container) or already counted on the physical uplink, so
/// summing it would double-count what the operator thinks of as "the
/// server's network I/O".
fn is_virtual_interface(name: &str) -> bool {
    name == "lo"
        || name.starts_with("veth")
        || name.starts_with("docker")
        || name.starts_with("br-")
        || name.starts_with("virbr")
}

/// Parse `/proc/net/dev` and sum rx/tx bytes over physical interfaces.
fn parse_net_dev(content: &str) -> Option<NetworkIoTotals> {
    let mut totals = NetworkIoTotals::default();
    let mut seen_any = false;
    // First two lines are headers. Data lines: `  eth0: <rx_bytes> <rx_packets>
    // <rx_errs> <rx_drop> <rx_fifo> <rx_frame> <rx_compressed> <rx_multicast>
    // <tx_bytes> ...`
    for line in content.lines().skip(2) {
        let (name, rest) = match line.split_once(':') {
            Some(v) => v,
            None => continue,
        };
        let name = name.trim();
        if is_virtual_interface(name) {
            continue;
        }
        let fields: Vec<&str> = rest.split_whitespace().collect();
        if fields.len() < 9 {
            continue;
        }
        let rx: u64 = fields[0].parse().ok()?;
        let tx: u64 = fields[8].parse().ok()?;
        totals.rx_bytes = totals.rx_bytes.saturating_add(rx);
        totals.tx_bytes = totals.tx_bytes.saturating_add(tx);
        seen_any = true;
    }
    seen_any.then_some(totals)
}

async fn collect_network_io(
    source_id: i32,
    config: &CollectorConfig,
    now: DateTime<Utc>,
) -> Vec<MetricPoint> {
    let content = match tokio::fs::read_to_string("/proc/net/dev").await {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let totals = match parse_net_dev(&content) {
        Some(t) => t,
        None => {
            debug!(
                source_id,
                "no physical network interfaces found in /proc/net/dev"
            );
            return Vec::new();
        }
    };
    network_io_points(source_id, config, totals, now)
}

fn network_io_points(
    source_id: i32,
    config: &CollectorConfig,
    totals: NetworkIoTotals,
    now: DateTime<Utc>,
) -> Vec<MetricPoint> {
    vec![
        gauge(
            source_id,
            config,
            "node.network_rx_bytes_total",
            totals.rx_bytes as f64,
            now,
        ),
        gauge(
            source_id,
            config,
            "node.network_tx_bytes_total",
            totals.tx_bytes as f64,
            now,
        ),
    ]
}

// ── Non-Linux fallback (sysinfo) ──────────────────────────────────────────────

#[cfg(not(target_os = "linux"))]
mod fallback {
    //! `sysinfo`-backed collection for hosts without `/proc`.
    //!
    //! Only the series the `/proc` readers above cannot produce off-Linux are
    //! filled in here: CPU %, memory, block I/O and network I/O. Disk space
    //! already works through `statvfs(2)`, and the fd / load-average series
    //! are deliberately left absent so the UI can say "Linux only" honestly.

    use sysinfo::{CpuRefreshKind, Disks, MemoryRefreshKind, Networks, RefreshKind, System};

    pub struct FallbackState {
        system: System,
        networks: Networks,
        disks: Disks,
        /// `sysinfo` reports 0% CPU on the very first refresh (no baseline);
        /// mirror the `/proc/stat` path and skip that sample.
        cpu_primed: bool,
    }

    impl FallbackState {
        pub fn new() -> Self {
            let system = System::new_with_specifics(
                RefreshKind::nothing()
                    .with_cpu(CpuRefreshKind::nothing().with_cpu_usage())
                    .with_memory(MemoryRefreshKind::nothing().with_ram()),
            );
            Self {
                system,
                networks: Networks::new_with_refreshed_list(),
                disks: Disks::new_with_refreshed_list(),
                cpu_primed: false,
            }
        }

        /// Refresh and return `(cpu_percent, memory_used, memory_total)`.
        /// `cpu_percent` is `None` on the first call.
        pub fn sample_cpu_memory(&mut self) -> (Option<f64>, u64, u64) {
            self.system.refresh_cpu_usage();
            self.system
                .refresh_memory_specifics(MemoryRefreshKind::nothing().with_ram());
            let cpu = if self.cpu_primed {
                Some(f64::from(self.system.global_cpu_usage()).clamp(0.0, 100.0))
            } else {
                self.cpu_primed = true;
                None
            };
            (cpu, self.system.used_memory(), self.system.total_memory())
        }

        /// Cumulative (rx, tx) bytes over non-virtual interfaces.
        pub fn sample_network(&mut self) -> Option<(u64, u64)> {
            self.networks.refresh(true);
            let mut rx = 0u64;
            let mut tx = 0u64;
            let mut seen = false;
            for (name, data) in self.networks.iter() {
                if super::is_virtual_interface(name)
                    // macOS names: loopback is `lo0`, Docker Desktop/colima
                    // bridges show up as `bridge*`/`utun*`/`vmenet*`.
                    || name.starts_with("lo")
                    || name.starts_with("bridge")
                    || name.starts_with("utun")
                    || name.starts_with("vmenet")
                    || name.starts_with("llw")
                    || name.starts_with("awdl")
                {
                    continue;
                }
                rx = rx.saturating_add(data.total_received());
                tx = tx.saturating_add(data.total_transmitted());
                seen = true;
            }
            seen.then_some((rx, tx))
        }

        /// Cumulative (read, write) bytes over listed disks.
        ///
        /// macOS mounts one APFS container several times (`/`,
        /// `/System/Volumes/Data`, …) and every mount reports the same
        /// physical counters, so volumes are de-duplicated by
        /// `(name, total_space)` — a stand-in for "same physical device".
        pub fn sample_disk_io(&mut self) -> Option<(u64, u64)> {
            self.disks.refresh(true);
            let mut read = 0u64;
            let mut write = 0u64;
            let mut seen_devices: Vec<(std::ffi::OsString, u64)> = Vec::new();
            for disk in self.disks.list() {
                let key = (disk.name().to_os_string(), disk.total_space());
                if seen_devices.contains(&key) {
                    continue;
                }
                seen_devices.push(key);
                let usage = disk.usage();
                read = read.saturating_add(usage.total_read_bytes);
                write = write.saturating_add(usage.total_written_bytes);
            }
            (!seen_devices.is_empty()).then_some((read, write))
        }
    }
}

#[cfg(not(target_os = "linux"))]
impl NodeMetricsCollector {
    fn collect_fallback(
        &self,
        source_id: i32,
        config: &CollectorConfig,
        now: DateTime<Utc>,
    ) -> Vec<MetricPoint> {
        let mut state = match self.fallback.lock() {
            Ok(g) => g,
            Err(e) => {
                warn!(source_id, "sysinfo fallback mutex poisoned: {e}");
                return Vec::new();
            }
        };
        let mut points = Vec::new();

        let (cpu, mem_used, mem_total) = state.sample_cpu_memory();
        if let Some(cpu) = cpu {
            points.push(gauge(source_id, config, "node.cpu_percent", cpu, now));
        }
        if mem_total > 0 {
            let percent = (mem_used as f64 / mem_total as f64) * 100.0;
            points.push(gauge(
                source_id,
                config,
                "node.memory_used_bytes",
                mem_used as f64,
                now,
            ));
            points.push(gauge(
                source_id,
                config,
                "node.memory_total_bytes",
                mem_total as f64,
                now,
            ));
            points.push(gauge(
                source_id,
                config,
                "node.memory_percent",
                percent,
                now,
            ));
        }
        if let Some((rx, tx)) = state.sample_network() {
            points.extend(network_io_points(
                source_id,
                config,
                NetworkIoTotals {
                    rx_bytes: rx,
                    tx_bytes: tx,
                },
                now,
            ));
        }
        if let Some((read, write)) = state.sample_disk_io() {
            points.extend(disk_io_points(
                source_id,
                config,
                DiskIoTotals {
                    read_bytes: read,
                    write_bytes: write,
                },
                now,
            ));
        }
        points
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn gauge(
    source_id: i32,
    config: &CollectorConfig,
    name: &str,
    value: f64,
    time: DateTime<Utc>,
) -> MetricPoint {
    MetricPoint {
        time,
        source_kind: SourceKind::Node,
        source_id,
        name: name.to_string(),
        value,
        kind: MetricKind::Gauge,
        engine: None,
        environment: config.environment.clone(),
        node_id: config.node_id,
        labels: HashMap::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn make_config(source_id: i32, data_dir: &str) -> CollectorConfig {
        CollectorConfig {
            source_id,
            source_kind: SourceKind::Node,
            connection_string: data_dir.to_string(),
            environment: None,
            node_id: Some(source_id),
            timeout: Duration::from_secs(5),
        }
    }

    // ── parse_meminfo_field ───────────────────────────────────────────────────

    #[test]
    fn test_parse_meminfo_kb() {
        let content = "MemTotal:       16384000 kB\nMemAvailable:   8192000 kB\n";
        assert_eq!(parse_meminfo_field(content, "MemTotal"), Some(16_384_000));
        assert_eq!(
            parse_meminfo_field(content, "MemAvailable"),
            Some(8_192_000)
        );
    }

    #[test]
    fn test_parse_meminfo_missing() {
        let content = "MemTotal: 1024 kB\n";
        assert_eq!(parse_meminfo_field(content, "MemFree"), None);
    }

    // ── read_cpu_snapshot ─────────────────────────────────────────────────────

    #[test]
    fn test_cpu_snapshot_no_panic() {
        // On Linux this reads a real file; on macOS it returns None gracefully.
        let snap = read_cpu_snapshot();
        // Whatever the result, no panic must occur.
        if let Some(s) = snap {
            assert!(s.total >= s.idle, "total must be >= idle");
        }
    }

    // ── collect_disk ─────────────────────────────────────────────────────────

    #[test]
    fn test_collect_disk_on_tmp() {
        let config = make_config(1, "/tmp");
        let now = Utc::now();
        let points = collect_disk(1, &config, Path::new("/tmp"), now);

        #[cfg(unix)]
        {
            assert_eq!(
                points.len(),
                3,
                "expected disk_used, disk_total, disk_percent"
            );
            let names: Vec<&str> = points.iter().map(|p| p.name.as_str()).collect();
            assert!(names.contains(&"node.disk_used_bytes"));
            assert!(names.contains(&"node.disk_total_bytes"));
            assert!(names.contains(&"node.disk_percent"));

            let total = points
                .iter()
                .find(|p| p.name == "node.disk_total_bytes")
                .unwrap();
            assert!(total.value > 0.0, "disk_total_bytes must be > 0");

            let pct = points
                .iter()
                .find(|p| p.name == "node.disk_percent")
                .unwrap();
            assert!((0.0..=100.0).contains(&pct.value), "percent out of range");
        }

        #[cfg(not(unix))]
        assert!(points.is_empty());
    }

    #[test]
    fn test_collect_disk_nonexistent_path_empty() {
        let config = make_config(1, "/this/does/not/exist/ever");
        let now = Utc::now();
        let points = collect_disk(1, &config, Path::new("/this/does/not/exist/ever"), now);
        assert!(points.is_empty());
    }

    // ── collect_system_fds ────────────────────────────────────────────────────

    #[test]
    fn test_collect_system_fds_on_linux() {
        if !Path::new("/proc/sys/fs/file-nr").exists() {
            return; // non-Linux: skip gracefully
        }

        let config = make_config(1, "/tmp");
        let now = Utc::now();
        let points = collect_system_fds(1, &config, now);

        assert_eq!(points.len(), 3, "expected fd_allocated, fd_max, fd_percent");
        let names: Vec<&str> = points.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains(&"node.fd_allocated"));
        assert!(names.contains(&"node.fd_max"));
        assert!(names.contains(&"node.fd_percent"));

        let max = points.iter().find(|p| p.name == "node.fd_max").unwrap();
        assert!(max.value > 0.0, "fd_max must be > 0");

        let pct = points.iter().find(|p| p.name == "node.fd_percent").unwrap();
        assert!((0.0..=100.0).contains(&pct.value), "percent out of range");
    }

    #[cfg(not(unix))]
    #[test]
    fn test_collect_system_fds_non_unix_empty() {
        let config = make_config(1, "/tmp");
        let points = collect_system_fds(1, &config, Utc::now());
        assert!(points.is_empty());
    }

    // ── collect_process_fds ───────────────────────────────────────────────────

    #[cfg(unix)]
    #[test]
    fn test_collect_process_fds_on_unix() {
        if !Path::new("/proc/self/fd").exists() {
            return; // unix without /proc (e.g. macOS): skip gracefully
        }

        let config = make_config(1, "/tmp");
        let now = Utc::now();
        let points = collect_process_fds(1, &config, now);

        assert!(
            points.iter().any(|p| p.name == "node.process_open_fds"),
            "process_open_fds must be present when /proc/self/fd exists"
        );
        let open = points
            .iter()
            .find(|p| p.name == "node.process_open_fds")
            .unwrap();
        assert!(open.value >= 1.0, "the test process itself has open fds");

        // The soft limit is present unless getrlimit(2) reports
        // RLIM_INFINITY, which is rare but legal — only assert the
        // percent's range when the limit metric is present.
        if let Some(pct) = points.iter().find(|p| p.name == "node.process_fd_percent") {
            assert!(pct.value >= 0.0, "percent must be non-negative");
        }
    }

    #[cfg(not(unix))]
    #[test]
    fn test_collect_process_fds_non_unix_empty() {
        let config = make_config(1, "/tmp");
        let points = collect_process_fds(1, &config, Utc::now());
        assert!(points.is_empty());
    }

    // ── NodeMetricsCollector integration ─────────────────────────────────────

    #[tokio::test]
    async fn test_first_scrape_no_cpu_metric() {
        let collector = NodeMetricsCollector::new();
        let config = make_config(1, "/tmp");
        let points = collector.collect(&config).await.unwrap();

        // First scrape: CPU baseline established but not emitted.
        let has_cpu = points.iter().any(|p| p.name == "node.cpu_percent");
        assert!(!has_cpu, "cpu_percent must not appear on the first scrape");
    }

    #[tokio::test]
    async fn test_second_scrape_has_cpu_on_linux() {
        if !Path::new("/proc/stat").exists() {
            return; // non-Linux: skip gracefully
        }

        let collector = NodeMetricsCollector::new();
        let config = make_config(1, "/tmp");
        let _ = collector.collect(&config).await.unwrap(); // establish baseline
        let points = collector.collect(&config).await.unwrap();

        let cpu = points
            .iter()
            .find(|p| p.name == "node.cpu_percent")
            .expect("cpu_percent must appear on the second scrape");
        assert!(
            (0.0..=100.0).contains(&cpu.value),
            "cpu_percent out of range"
        );
    }

    #[tokio::test]
    async fn test_memory_metrics_on_linux() {
        if !Path::new("/proc/meminfo").exists() {
            return;
        }

        let collector = NodeMetricsCollector::new();
        let config = make_config(1, "/tmp");
        let points = collector.collect(&config).await.unwrap();

        let total = points.iter().find(|p| p.name == "node.memory_total_bytes");
        assert!(
            total.is_some(),
            "memory_total_bytes should be present on Linux"
        );
        assert!(total.unwrap().value > 0.0);
    }

    #[tokio::test]
    async fn test_loadavg_metrics_on_linux() {
        if !Path::new("/proc/loadavg").exists() {
            return;
        }

        let collector = NodeMetricsCollector::new();
        let config = make_config(1, "/tmp");
        let points = collector.collect(&config).await.unwrap();

        let names: Vec<&str> = points.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains(&"node.load_avg_1m"));
        assert!(names.contains(&"node.load_avg_5m"));
        assert!(names.contains(&"node.load_avg_15m"));
    }

    #[tokio::test]
    async fn test_all_node_metrics_have_correct_source_kind() {
        let collector = NodeMetricsCollector::new();
        let config = make_config(5, "/tmp");
        let points = collector.collect(&config).await.unwrap();

        for p in &points {
            assert_eq!(
                p.source_kind,
                SourceKind::Node,
                "metric {} has wrong source_kind",
                p.name
            );
            assert_eq!(p.source_id, 5);
        }
    }
    // ── parse_diskstats ───────────────────────────────────────────────────────

    #[test]
    fn test_parse_diskstats_sums_whole_devices_and_skips_partitions_and_virtual() {
        // sda: 1000 sectors read, 2000 written; sda1 is a partition of sda
        // (must be skipped); nvme0n1 + nvme0n1p1 likewise; loop0 and dm-0
        // are virtual.
        let content = "\
   8       0 sda 100 0 1000 0 200 0 2000 0 0 0 0 0 0 0 0 0 0
   8       1 sda1 90 0 900 0 190 0 1900 0 0 0 0 0 0 0 0 0 0
 259       0 nvme0n1 10 0 10 0 20 0 20 0 0 0 0 0 0 0 0 0 0
 259       1 nvme0n1p1 9 0 9 0 19 0 19 0 0 0 0 0 0 0 0 0 0
   7       0 loop0 5 0 50000 0 5 0 50000 0 0 0 0 0 0 0 0 0 0
 253       0 dm-0 5 0 50000 0 5 0 50000 0 0 0 0 0 0 0 0 0 0
";
        let totals = parse_diskstats(content).expect("should parse");
        assert_eq!(totals.read_bytes, (1000 + 10) * 512);
        assert_eq!(totals.write_bytes, (2000 + 20) * 512);
    }

    #[test]
    fn test_parse_diskstats_only_virtual_devices_is_none() {
        let content = "   7       0 loop0 5 0 50000 0 5 0 50000 0 0 0 0 0 0 0 0 0 0\n";
        assert!(parse_diskstats(content).is_none());
    }

    #[test]
    fn test_parse_diskstats_short_lines_ignored() {
        assert!(parse_diskstats("garbage\n\n").is_none());
    }

    // ── parse_net_dev ─────────────────────────────────────────────────────────

    #[test]
    fn test_parse_net_dev_sums_physical_interfaces_only() {
        let content = "\
Inter-|   Receive                                                |  Transmit
 face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed
    lo: 999999 100 0 0 0 0 0 0 999999 100 0 0 0 0 0 0
  eth0: 1000 10 0 0 0 0 0 0 2000 20 0 0 0 0 0 0
 veth1a2b: 555 5 0 0 0 0 0 0 666 6 0 0 0 0 0 0
docker0: 777 7 0 0 0 0 0 0 888 8 0 0 0 0 0 0
  wlan0: 30 3 0 0 0 0 0 0 40 4 0 0 0 0 0 0
";
        let totals = parse_net_dev(content).expect("should parse");
        assert_eq!(totals.rx_bytes, 1000 + 30);
        assert_eq!(totals.tx_bytes, 2000 + 40);
    }

    #[test]
    fn test_parse_net_dev_only_loopback_is_none() {
        let content = "h1\nh2\n    lo: 1 1 0 0 0 0 0 0 1 1 0 0 0 0 0 0\n";
        assert!(parse_net_dev(content).is_none());
    }

    #[test]
    fn test_io_metric_names_are_monotonic_counters() {
        // The read path keys the LAG-based "increase per bucket" query off
        // the `_total` suffix; a rename here would silently turn the I/O
        // charts into cumulative-since-boot lines.
        for name in [
            "node.disk_read_bytes_total",
            "node.disk_write_bytes_total",
            "node.network_rx_bytes_total",
            "node.network_tx_bytes_total",
        ] {
            assert!(crate::is_monotonic_counter(name), "{name}");
        }
    }

    #[tokio::test]
    async fn test_io_metrics_present_on_linux() {
        let collector = NodeMetricsCollector::new();
        let config = make_config(1, "/tmp");
        let points = collector.collect(&config).await.unwrap();
        let names: Vec<&str> = points.iter().map(|p| p.name.as_str()).collect();
        if std::path::Path::new("/proc/net/dev").exists() {
            assert!(names.contains(&"node.network_rx_bytes_total"), "{names:?}");
            assert!(names.contains(&"node.network_tx_bytes_total"), "{names:?}");
        }
        // Block devices may legitimately be absent inside some sandboxes, so
        // only assert the invariant that both or neither are emitted.
        assert_eq!(
            names.contains(&"node.disk_read_bytes_total"),
            names.contains(&"node.disk_write_bytes_total")
        );
    }

    #[cfg(not(target_os = "linux"))]
    #[tokio::test]
    async fn test_fallback_fills_memory_and_cpu_off_linux() {
        let collector = NodeMetricsCollector::new();
        let config = make_config(1, "/tmp");
        // First scrape primes the CPU baseline; second carries cpu_percent.
        let _ = collector.collect(&config).await.unwrap();
        tokio::time::sleep(Duration::from_millis(250)).await;
        let points = collector.collect(&config).await.unwrap();
        let names: Vec<&str> = points.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains(&"node.memory_used_bytes"), "{names:?}");
        assert!(names.contains(&"node.memory_total_bytes"), "{names:?}");
        assert!(names.contains(&"node.cpu_percent"), "{names:?}");
    }
}
