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
//!
//! ## Non-Linux behaviour
//!
//! When `/proc` is absent (macOS, FreeBSD, Windows) every `/proc` read
//! degrades gracefully: the collector returns whatever metrics it could collect
//! and silently skips the rest.  No error is propagated.

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
}

impl NodeMetricsCollector {
    pub fn new() -> Self {
        Self {
            prev_cpu: Mutex::new(None),
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

        let bsize = buf.f_bsize as u64;
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
}
